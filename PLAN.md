# sanitty: plan

A PyCharm-flavoured terminal IDE built on the published `karet-*` crates. It runs on
the office PC over SSH (from Ghostty, sometimes the T3 integrated terminal), so files,
language servers, git, docker and databases are all local to it.

Status: approved plan; shortcuts are starting defaults. Milestones 1 to 4 are done;
milestone 5 (Polish) is built: the settings file and multi-caret editing.

## Principles

- One full-screen view at a time. Everything else (file finder, project tree, usages,
  actions) is a popup that disappears when you are done with it.
- Not modal. One key per action, built on Alt: T3's terminal claims many Ctrl keys and
  may claim more, so only the universal Ctrl keys (copy, cut, paste, undo and redo
  (Ctrl+Shift+Z, next to undo), save, select all, find) and PyCharm's F-keys remain. Every default arrives in both T3 and Ghostty.
- Keyboard only. Mouse capture stays off so the terminal keeps normal text selection.
- Everything is rebindable in `~/.config/sanitty/keymap.toml`.

## Views

Three top-level views, each taking the whole screen.

| View     | Key     | Purpose                                      |
|----------|---------|----------------------------------------------|
| Code     |         | Editing and navigation (default)             |
| Review   | `Alt+9` | Diffs and review comments                    |
| Database | `Alt+8` | Schema, queries, result grids, row editing   |

A view's key opens it and, pressed again, goes back to the code. The Database view sits
on `Alt+8`, next to Review (Alt+D, planned earlier, is now Duplicate Line).

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
- Keys inside the view are plain letters (nothing is typed into a diff; held with Ctrl or
  Alt they do nothing, so Alt+D never deletes a comment). Going to a file, the tree or a
  search (Alt+N, Alt+E, Alt+K, Alt+1, ...) leaves the view; editing and code features say
  they do not apply; the same rule table as the Database view. The view's letters: Up/Down move,
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
  repo. Its name holds a fixed hash of the repository path (FNV-1a, the same in every
  build; state under the names earlier builds used is moved over). It is written in one
  step, merged with what another sanitty on the same repository wrote meanwhile (both
  keep their comments, deletions stay deleted), and a file that cannot be read is kept
  aside as `...broken-<time>.json` with a message instead of being replaced.

### Database view

```
 Database   db (postgres, app@127.0.0.1:5460/app, from docker compose: db)   76 tables
+- schema ---------++- db.sql ------------------------------------ Alt+Enter run -+
| > audit          || select id, email, created_at from auth_user                |
| v public         || where created_at > now() - interval '1 day';               |
|   > auth_user    |+------------------------------------------------------------+
|   > shop_order   |+- 3 rows, 12 ms ---------------------- Enter edits a cell -+
|   > shop_product ||   id | email               | created_at                    |
|                  || 1 41 | a@example.nl        | 2026-11-19 09:12              |
+------------------++------------------------------------------------------------+
 Enter Open table  Right/Left Expand  s Select in query  / Filter  c Connections
```

- Engines: PostgreSQL, MySQL/MariaDB, SQLite (one driver layer, `sqlx`). SQL runs over
  each server's text protocol, so several statements run at once and every value shows
  as text (NULL dimmed, bytes as `\x..` hex). A result keeps at most 10,000 rows. The
  grid shows the last statement's result; when that is a read that found nothing, its
  column headers come from preparing it (no extra run).
- Connections: discovered from `docker compose config`: a service counts by its image
  (postgres, postgis, timescaledb, pgvector, mysql, mariadb, percona) or, for images built
  locally, by the server's own `POSTGRES_*` / `MYSQL_*` / `MARIADB_*` variables (clients
  with only a `POSTGRES_URI` do not). A published port is used on the host; an unpublished
  one through the running container's address. `.sanitty/db.toml` overrides a service
  (an entry with its name) or adds connections (`url`, or host/port/user/database, or a
  SQLite `path`; `password_env` reads the password from the environment).
- Opening: `Alt+8` finds the connections the first time and connects to the one used
  last, or the only one; otherwise a list asks. `c` switches, `r` reads the schema again.
- Panes: schema tree, query editor, result grid; `Tab` moves between them (in the query
  editor, where Tab indents, `Shift+Tab`). The tree hides the schema level when there is
  only one; `/` filters tables by name, Enter opens a table's rows (500, by primary key),
  `s` puts `select * from` it in the query.
- Copying in the tree and grid: `y` copies the name or the value under the cursor;
  `Ctrl+C` leaves the view, as everywhere. (Ctrl+Shift+C was tried: T3 takes it before
  its terminal sees it, so no program in T3's terminal can use it.)
- Query editor: a scratch `.sql` file per connection in `~/.local/state/sanitty/`, edited
  with all the usual keys, saved when leaving the view. `Alt+Enter` runs the selection or
  the statement under the caret (a caret right after a `;` still means that statement).
- SQL help from the schema (there is no SQL language server), in the query editor and in
  `.sql` files once connected: `Alt+J` completes columns of the statement's tables
  (`alias.` and `table.` narrow to one table, `schema.` to its tables), table names and
  keywords; `Alt+H` shows a table's definition or a column's type; resting the caret on a
  name shows its type in the bottom line. The hint bar shows these keys for SQL.
- Keys in the view: editing and SQL help work in the query editor; going elsewhere (Go to
  File, Recent Files, the tree, Review, ...) goes back to the code first; code features
  that mean nothing for SQL (definition, usages, rename, reformat, problems, close file)
  say so in the status line and stay.
- Safety: every write (anything but select/show/explain/values/with-without-writes, so
  unknown statements too) shows the exact SQL and asks for confirmation. Cells of any
  result can be edited (a table opened from the tree, `select *`, some columns, a join)
  when the server says the column is a table's (renamed columns too, computed ones not)
  and that table's primary key is in the result as well; otherwise Enter says what is
  missing. Enter edits the cell (Tab makes it NULL), the generated
  `UPDATE ... WHERE pk = ...` is shown first, and the query runs again after (only when
  it only reads; a write never runs again by itself, and `r` asks first). DDL reads the
  schema again.
- What counts as a read is decided per engine: a backslash escapes a quote only in MySQL
  and Postgres `E'...'` strings, MySQL's `/*! ... */` is code, not a comment, and a SQLite
  `PRAGMA` with an argument only reads when it is one that reports (`table_info`, ...).
- A cell edit changes one row or nothing: its `UPDATE` runs in a transaction that is undone
  unless exactly one row changed. It is refused up front when a key value is NULL, when
  the table appears more than once in the query (a self-join: the server names the table,
  not which use of it), or when a key column is in the result twice.
- The confirmation box shows all of the SQL: long batches scroll, and Enter only runs it
  once every line has been on screen. Only a plain Enter or `y` confirms (Alt+Y, Delete
  Line, does nothing there). The Database view and Run SQL over the code share it.
- From the Code view, `Alt+Enter` in any file runs the selection (or the statement under
  the caret) against the active connection and shows the grid in a bottom popup (writes are
  confirmed first). Without a connection it says so; `Alt+8` makes one.

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
| Database view (and back) | `Alt+8`          |
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
| Run SQL                  | `Alt+Enter`      |
| Duplicate line           | `Alt+D`          |
| Delete line              | `Alt+Y`          |
| Add caret above / below  | `Alt+Up` / `Alt+Down` |
| Add next occurrence      | `Alt+A`          |
| Remove last added caret  | `Alt+U`          |
| Select all occurrences   | `Ctrl+Alt+J`     |
| Carets at line ends      | `Ctrl+Alt+G`     |
| Undo / redo              | `Ctrl+Z` / `Ctrl+Shift+Z` |
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

- Alt: 2, 3, 4, 5, 7, 0, `-`, `=`, Backspace; O, P, `[`, `]`, `\`; `;`, `'`,
  Z, X, C, V, M
- Ctrl+Alt: Q, W, E, R, T, Y, U, I, O, P; S, D, F, H, K, L; Z, X, C, V, N, M;
  Shift+N; Up, Down, Left, Right
- F-keys: F4, F5, F7, F9, F10, F11, F12; Shift+F1, F2, F4, F5, F7, F9, F10, F11, F12;
  Ctrl+F1 to F5, F7 to F12; Alt+F1, F2, F3, F5, F6, F8 to F12
- Ctrl (avoid, see above): E, R, T, Y, U, O, P, `]`, `\`, D, G, K, L, `;`, `'`, `.`, `/`,
  PageUp, PageDown, Up, Down
- Ctrl+Shift (last resort: older terminals fold it into Ctrl+letter): R, Y, S, G, H, K,
  L, X, B, M
- Shift: Tab, Enter, Delete

Ctrl+I, Ctrl+M, Ctrl+H and Ctrl+[ also arrived in both, but only because both speak the
kitty keyboard protocol: everywhere else (tmux, older terminals) they are Tab, Enter,
Backspace and Esc, so they are left out. Not usable: Ctrl+Space, F6 and Ctrl+Shift+C (T3
swallows them; Ctrl+Shift+C reaches no program in T3's terminal, checked 2026-11-26),
Alt+Left/Right (arrive as Alt+B/F in T3), Ctrl+digits (T3), Ctrl+Enter, Ctrl+`,`,
Ctrl+0, Ctrl+`-`, Ctrl+`=` (Ghostty), Alt+F4 (closes the window), Alt+Space (opens
Espanso on Robin's desktop, though it arrived in both terminals).

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
  When ruff is not installed, Python runs on ty alone, silently. ty is still beta, so
  basedpyright stays a fallback, and `settings.toml` can prefer it (or pyright, pylsp) or
  turn ruff off. Rust uses rust-analyzer. TypeScript/JavaScript use
  TypeScript 7's own server (`tsc --lsp --stdio`, the project's `node_modules` first,
  else the `tsc` on PATH, e.g. from `bun i -g typescript`), with
  typescript-language-server as the fallback for older TypeScript (pointed at a global
  `tsserver.js` when the project has none). No `package.json` is needed. Servers run on the host with the project's own venv.
- `karet-diff` (with its `view` feature) and `karet-vcs`: diffs and git facts.
- `sqlx`: databases.
- Own code: views, popups, keymap, config, review model, compose discovery.

Config lives in `~/.config/sanitty/` (keymap, settings) and per project in
`.sanitty/` (db connections). Review state lives in `~/.local/state/sanitty/`.

## Milestones

1. **Editor shell** (done): editing, highlighting, go to file, recent
   files, previous file, project tree, find action, find and replace, find in files,
   keymap file, shell, rainbow CSV.
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
4. **Database** (done): compose discovery, schema tree, query editor,
   result grid, confirmed writes and cell edits, `Alt+Enter` from `.sql` files. Tested
   against Postgres (compose-built image), MariaDB and MySQL (published and unpublished
   ports) and SQLite. A read that finds no rows still shows its column headers (the
   server prepares the statement for them), and MySQL's `#` comments are recognised on
   MySQL connections only.
5. **Polish** (built, awaiting demo): multi-caret, settings file.
   - Multi-caret (built): `Alt+Up`/`Alt+Down` add a caret above/below, `Alt+A` adds the
     next occurrence of the word under the caret (the first press selects it) or of the
     selection, `Alt+U` removes the last added caret (walks back caret operations),
     `Ctrl+Alt+J` selects all occurrences, `Ctrl+Alt+G` puts a caret at the end of each
     selected line, `Esc` goes back to one. Typing, Backspace/Delete (words too), Enter,
     Tab, paste, cut, duplicate/delete line and toggle comment work at every caret as one
     undo step. Copy and cut join several selections with line breaks; a paste with as
     many lines as carets puts one at each. Completion, rename, hover and jumps use the
     main caret (the last added) and the other carets stay where they are. The top bar
     shows `3 carets`. Without the kitty keyboard protocol (tmux, older terminals)
     Ctrl+Alt+J arrives as Alt+Enter (Run SQL); T3 and Ghostty send it as itself.
   - Settings file (done): `~/.config/sanitty/settings.toml`, read at start like the
     keymap; every key optional, mistakes shown in the status line with the default kept.
     `[editor] tab_size`; `[python] server` (ty, basedpyright, pyright, pylsp: the
     preferred one first, then the usual order) and `ruff`; `[database] table_rows` and
     `max_rows`.

Each milestone ends with a short demo for approval before the next one starts.
