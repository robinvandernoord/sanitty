# sanitty: plan

A PyCharm-flavoured terminal IDE built on the published `karet-*` crates. It runs on
the office PC over SSH (from Ghostty, sometimes the T3 integrated terminal), so files,
language servers, git, docker and databases are all local to it.

Status: approved plan; shortcuts are starting defaults. Milestone 1 is built.

## Principles

- One full-screen view at a time. Everything else (file finder, project tree, usages,
  actions) is a popup that disappears when you are done with it.
- Not modal. One key per action, built on Alt: T3's terminal claims many Ctrl keys and
  may claim more, so only the universal Ctrl keys (copy, cut, paste, undo, save, select
  all, find) and PyCharm's F-keys remain. Every default arrives in both T3 and Ghostty.
- Keyboard only. Mouse capture stays off so the terminal keeps normal text selection.
- Everything is rebindable in `~/.config/sanitty/keymap.toml`.

## Views

Three top-level views, each taking the whole screen.

| View     | Key     | Purpose                                      |
|----------|---------|----------------------------------------------|
| Code     |         | Editing and navigation (default)             |
| Review   | `Alt+9` | Diffs and review comments                    |
| Database | (later) | Schema, queries, result grids, row editing   |

A view's key opens it and, pressed again, goes back to the code. The Database key is
chosen in milestone 4 (Alt+D, planned earlier, is now Duplicate Line).

### Code view

No tab line. `Alt+E` (recent files) is the file switcher; `Alt+E Enter` jumps back to
the previous file. Open buffers stay alive in the background, so unsaved changes are
never lost by switching.

```
 src/app.rs *   2 errors  3 warnings   |  1 other file with errors          rust
  12 | fn main() {
  13 |     let x: u32 = "text";   mismatched types: expected u32, found &str
  14 | }
 mismatched types: expected u32, found &str                                13:18
 Alt+K Search   F8 Next problem   Ctrl+F Find   Alt+B Definition   F1 All keys
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

A list of changed files, a diff, line comments, and export. Read-only: nothing is edited
here.

```
 Review   working tree vs HEAD   3 files  +41 -12   2 comments
+- files ------------++- apps/shop/models.py ----------------------- split -+
| v models.py   +12  || 40   def total(self):      | 40   def total(self):  |
|   views.py    +27  || 41 -     return sum(...)   | 41 +     return sum(   |
|   urls.py      +2  ||                            | 42 +       l.price ... |
|                    ||  [issue] use Decimal here, floats round wrong        |
+--------------------++------------------------------------------------------+
 c Comment  v Viewed  \ Split/inline  ] [ File  y Copy review  Alt+9 Code
```

- Opening: `Alt+9` shows the source picker the first time, then the last source; `s`
  picks another. The same key goes back to the code.
- Sources, from the picker: your changes (working tree vs HEAD), this branch vs main,
  commits (a filterable list of the last 200; Space picks several, and the review then
  covers everything from the oldest to the newest picked, saying so when that includes
  commits you did not pick), and an open GitHub pull request (a filterable list through
  `gh`; its commits are fetched for the syntax colours when they are not local yet).
  Each commit range and PR keeps its own comments.
- Layout: side-by-side, switching to inline when the terminal is narrow; `\` toggles.
- Keys inside the view are plain letters (nothing is typed into a diff): Up/Down move,
  Tab switches between the file list and the diff, Shift+Up/Down select a range,
  `c` comments on the line or range, Enter on a comment edits it, `d` deletes it, `v`
  marks the file viewed, `]`/`[` go to the next/previous file, `y` copies the review,
  `x` discards the review (asks first): its comments and viewed marks go, and the
  source picker opens; Alt+9 then asks again too. Enter on a diff line does nothing.
- Comments on a line or range, typed as issue / suggestion / note / praise (Tab cycles
  the type in the comment box).
- Export (`y`): the whole review as Markdown to the clipboard (OSC 52, works over SSH),
  grouped by file, each comment with the diff lines it is about:

  ````
  ## apps/shop/models.py

  **issue** (lines 41-42): use Decimal here, floats round wrong

  ```diff
  -     return sum(...)
  +     return sum(
  +       l.price ...
  ```
  ````
- Comments and viewed marks are kept across restarts until the review is discarded. A
  comment stays on its line while that line's text is still in the diff.
- State lives outside the project, in `~/.local/state/sanitty/`, one file per
  repository (both sources, and which one was used last), so nothing shows up in the
  repo.

### Database view

```
 [Database]  shop-db (postgres, from docker compose: db)
+- schema ---------++- query.sql ----------------------------------------------+
| v public         || select id, email, created_at from auth_user              |
|   > auth_user    || where created_at > now() - interval '1 day';             |
|   > shop_order   |+----------------------------------------- Alt+Enter run -+
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
- From the Code view, `Alt+Enter` in a `.sql` file runs the selection (or the statement
  under the caret) against the active connection and shows the grid in a bottom popup.

### Terminal

`Alt+T` suspends sanitty and hands the terminal to `$SHELL` in the workspace root;
exiting the shell returns. No embedded terminal for now.

## Finding your way

- **Hint bar**: the bottom line shows the keys that matter right now and changes with
  context (editor, find bar, launcher, tree, rename). It shows one key per action: the
  key, else the launcher route (`Alt+K >`) for actions without one.
- **Launcher** (`Alt+K`, Search Everywhere): files (recent first; build output, caches
  and virtualenvs are never listed), actions with their
  key, workspace symbols (from the language server) and text in files, all in one list.
  `Tab` cycles the filters All / Files / Actions / Symbols / Text, and the prefixes `>`
  actions, `@` symbols, `/` text, `:` line narrow the same way; neither is ever needed,
  since All searches everything. Every action is reachable here (this replaces PyCharm's
  Find Action), which is the safety net for keys a terminal swallows. Ranking prefers
  names that start with or contain the query, and actions also match synonyms ("jump to
  source", "references"); letters scattered through a long name rank last.
- **Find bar** (`Ctrl+F`, `Alt+R` for replace): incremental, smart case, toggles for
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
  with the project tree open on that file (files stay open, `Alt+E` brings them
  back); on the home screen it quits, also while the tree is open. The tree always
  reopens as it was left: same open folders, same selection.
- **Style files**: a swatch in the actual colour in front of every `#hex`, `rgb()` and
  `hsl()` value (CSS, SCSS, Sass, Less, HTML, Vue, Svelte). SCSS and Less are
  highlighted with the CSS grammar (karet's own SCSS rules colour almost nothing), plus
  their `$`/`@` variables.
- **Types and docs**: resting the caret on a name shows its type in the bottom line
  (`g: Greeter`) when there is no message or problem to show. `Alt+H` opens a panel
  under the caret with the full signature and docstring; any key closes it.
- **Code completion** (`Alt+J`, never by itself): a list under the word with a kind
  letter, the name and its type, and the selected item's docs beside it (below it on a
  narrow screen). Typing narrows it (names that start with what was typed first, then
  fuzzy matches); Up/Down select, Enter inserts, Esc closes, Tab indents and closes it.
  Enter replaces the whole word around the caret, adds an import the item needs
  (`from pathlib import Path`, `use std::collections::HashMap;`) in the same undo
  step, and puts the caret in a snippet's first placeholder, selected.
- **All keys** (`F1`): every action grouped, filterable; Enter runs it.
- **Check keys** (in the launcher): asks for each default key in turn and records whether
  it arrives, arrives as something else, or is swallowed. Writes
  `~/.config/sanitty/keycheck-<terminal>.md` and copies it. The results decide the final
  keymap; no automatic per-terminal profiles, so muscle memory stays the same everywhere.

## Key bindings (defaults)

One key per action. Every key below arrived in both T3's terminal and Ghostty in the
key check (`sanitty --check-keys`, 2026-11-19).

| Action                   | Key              |
|--------------------------|------------------|
| Search everywhere        | `Alt+K`          |
| Go to file               | `Alt+N`          |
| Go to symbol             | `Alt+S`          |
| Go to line               | `Alt+G`          |
| Recent files             | `Alt+E` (`Enter` = previous file) |
| Project tree             | `Alt+1`          |
| Review view (and back)   | `Alt+9`          |
| Navigate back / forward  | `Alt+,` / `Alt+.` |
| Find in file             | `Ctrl+F`         |
| Replace in file          | `Alt+R`          |
| Find next / previous     | `F3` / `Shift+F3` |
| Find in files            | `Alt+F`          |
| Go to definition         | `Alt+B`          |
| Go to implementation     | `Alt+I`          |
| Find usages              | `Alt+F7`         |
| Quick documentation      | `Alt+H`          |
| Rename                   | `F2`             |
| Problems (all files)     | `Alt+6`          |
| Next / previous problem  | `F8` / `Shift+F8` (crosses files) |
| Comment line             | `Alt+/`          |
| Reformat file            | `Alt+L`          |
| Code completion          | `Alt+J`          |
| Duplicate line           | `Alt+D`          |
| Delete line              | `Alt+Y`          |
| Undo / redo              | `Ctrl+Z` / `Alt+Z` |
| Copy (quits without selection) / cut / paste | `Ctrl+C` / `Ctrl+X` / `Ctrl+V` |
| Select all               | `Ctrl+A`         |
| Save                     | `Ctrl+S`         |
| Close file               | `Alt+W`          |
| All keys                 | `F1`             |
| Shell                    | `Alt+T`          |
| Quit                     | `Alt+Q`          |

Copy Line and Check Keys have no key; they are in the launcher. Find Action is not a
separate action: actions show up in the launcher's All results, and `>` only narrows to
them. Inside the find bar, `Alt+C` / `Alt+W` / `Alt+X` toggle case, words and regex.

### Free keys

128 keys arrive in both terminals and are bound to nothing; these are the pool for new
bindings. Prefer Alt, then Ctrl+Alt.

- Alt: 2, 3, 4, 5, 7, 8, 0, `-`, `=`, Backspace; U, O, P, `[`, `]`, `\`; A, `;`, `'`,
  Enter (planned: run SQL); X, C, V, M; Up, Down
- Ctrl+Alt: Q, W, E, R, T, Y, U, I, O, P; S, D, F, G, H, J, K, L; Z, X, C, V, N, M;
  Shift+N; Up, Down, Left, Right
- F-keys: F4, F5, F7, F9, F10, F11, F12; Shift+F1, F2, F4, F5, F7, F9, F10, F11, F12;
  Ctrl+F1 to F5, F7 to F12; Alt+F1, F2, F3, F5, F6, F8 to F12
- Ctrl (avoid, see above): E, R, T, Y, U, O, P, `]`, `\`, D, G, K, L, `;`, `'`, `.`, `/`,
  PageUp, PageDown, Up, Down
- Ctrl+Shift (last resort: older terminals fold it into Ctrl+letter): R, Y, S, G, H, K,
  L, Z, X, C, B, M
- Shift: Tab, Enter, Delete

Ctrl+I, Ctrl+M, Ctrl+H and Ctrl+[ also arrived in both, but only because both speak the
kitty keyboard protocol: everywhere else (tmux, older terminals) they are Tab, Enter,
Backspace and Esc, so they are left out. Not usable: Ctrl+Space and F6 (T3 swallows
them), Alt+Left/Right (arrive as Alt+B/F in T3), Ctrl+digits (T3), Ctrl+Enter,
Ctrl+`,`, Ctrl+0, Ctrl+`-`, Ctrl+`=` (Ghostty), Alt+F4 (closes the window), Alt+Space
(opens Espanso on Robin's desktop, though it arrived in both terminals).

## Architecture

One Rust binary.

- `karet-text`, `karet-editor`, `karet-syntax`, `karet-theme`: buffers, the editor widget,
  tree-sitter highlighting. A file keeps its syntax tree, so an edit reparses and
  recolours only what it touched (a keystroke in a 435 KB Python file: about 2 ms
  instead of 160 ms).
- `karet-lsp`: language servers. One per language, started on first use. Python uses
  Astral's ty (`ty server`) for types and navigation, with ruff (`ruff server`) alongside
  for lint and format: each server's problems are kept apart and shown merged, and
  Reformat asks the first server that can format (ruff for Python).
  When ruff is not installed, Python runs on ty alone, silently. ty is still beta, so basedpyright stays a configured fallback and
  the choice is a setting. Rust uses rust-analyzer. TypeScript/JavaScript use
  TypeScript 7's own server (`tsc --lsp --stdio`, the project's `node_modules` first,
  else the `tsc` on PATH, e.g. from `bun i -g typescript`), with
  typescript-language-server as the fallback for older TypeScript (pointed at a global
  `tsserver.js` when the project has none). No `package.json` is needed. Servers run on the host with the project's own venv.
- `karet-diff` (with its `view` feature) and `karet-vcs`: diffs and git facts.
- `sqlx`: databases.
- Own code: views, popups, keymap, config, review model, compose discovery.

Config lives in `~/.config/sanitty/` (keymap, theme, settings) and per project in
`.sanitty/` (db connections). Review state lives in `~/.local/state/sanitty/`.

## Milestones

1. **Editor shell** (done, awaiting demo): editing, highlighting, go to file, recent
   files, previous file, project tree, find action, keymap file, shell, rainbow CSV.
2. **LSP** (done): definition, implementation, usages, rename, problems (ty and
   ruff for Python), quick documentation, reformat and code completion work; ty is the
   default Python server. Large-repo check done: a 54k-file tree indexes in 0.3 s, the launcher
   filters in about 10 ms per key, and a 20 MB CSV opens and edits without lag.
   Known limit: Rust files of several MB with doc
   comments still take about 0.5 s per keystroke (karet rebuilds embedded layers on
   every edit).
3. **Review** (done): the Review view with four sources (your changes, this branch vs
   main, commits, a GitHub PR), split and inline diffs in syntax colours, line and range
   comments, viewed marks, Markdown export, state kept between runs.
4. **Database**: compose discovery, schema tree, query editor, result grid, confirmed
   writes, `Alt+Enter` from `.sql` files.
5. **Polish**: search in files, multi-caret, VS Code theme loading, settings file.

Each milestone ends with a short demo for approval before the next one starts.
