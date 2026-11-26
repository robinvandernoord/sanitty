# Changelog

<!-- next-version-placeholder -->

## v0.1.0 (2026-11-26)

The first release of sanitty, a non-modal IDE for the terminal, built to run over SSH and in terminals inside other apps.

### Features
* **editor:** editing with tree-sitter highlighting, find and replace, find in files, Search Everywhere, go to file, symbol and line, recent files and a project tree
* **editor:** multi-caret editing: carets above and below, at the next or every occurrence, and at the ends of selected lines
* **lsp:** language servers for Python (ty and ruff), Rust and TypeScript/JavaScript: definition, usages, rename, problems, documentation, formatting and completion
* **review:** a Review view (Alt+9) for uncommitted changes, a branch, commits or a GitHub pull request, with line comments, viewed marks and Markdown export
* **database:** a Database view (Alt+8) for PostgreSQL, MySQL/MariaDB and SQLite, found in docker compose: schema tree, query editor with completion, result grid, and cell editing; every write is shown and confirmed first
* **csv:** rainbow columns and structure checks for CSV and TSV files
* **config:** rebindable keys (`keymap.toml`) and settings (`settings.toml`)
