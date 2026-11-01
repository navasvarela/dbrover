# Persistence

State lives in two folders, the same on every platform (H4, see
[Settings folders](settings-folders.md)): the **settings folder**, `~/.config/dbrover`, and the
**data folder**, `~/.local/share/dbrover` (under `%USERPROFILE%` on Windows; on Linux
`$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` move them). `--config-dir`, `DBROVER_CONFIG_DIR` or the
folder chosen in Settings › Files replaces the settings folder; the data folder never moves.
`dbrover_store::AppPaths` names every path.

```
~/.config/dbrover/            # the settings folder
├── data_sources.toml     # data source configs, no secrets
├── projects.toml         # projects and their MCP permissions
├── settings.toml         # app settings: SQL formatting (more later: page size, fonts)
├── keymap.toml           # M5, key bindings (see features/keymap.md)
├── actions.toml          # H1/H2, custom SQL and command actions (see hackability.md)
├── snippets.toml         # H1, console snippets
├── theme.toml            # H1, colours
├── consoles/             # M4
│   └── <data-source-id>/<console-name>.sql
├── diagrams/             # ER1, ER diagrams
│   └── <project-slug or _none>/<diagram-name>.toml
└── history.sqlite        # M4, query history

~/.local/share/dbrover/       # the data folder: this machine's own
├── app.ron               # eframe storage: window size, egui_dock layout, open tabs
├── trusted_commands.toml # H2 and W9, commands trusted to run (see below)
├── settings_folder.toml  # H4, the settings folder chosen in Settings, if any
└── logs/dbrover.<date>.log # daily rolling file log, last 7 kept
```

Up to 0.21, macOS kept both in `~/Library/Application Support/dbrover` and Windows in
`%APPDATA%\dbrover\config` and `\data`; the first start of a later version copies them (see
"Migrating existing installs" in [Releasing](releasing.md)).

`trusted_commands.toml` lists each command action the user chose **Run** for, by its id,
command as written and stdin, and each data source's password command (W9) the user chose
**Run and connect** for, by the data source's id and the command as written. The engine writes
it atomically. It is kept apart from `actions.toml` and `data_sources.toml` so a settings folder
copied from elsewhere, or an imported data source, brings its commands but no trust: each
command asks before its first run on every machine. A file that can't be read trusts nothing.
`dbrover mcp` reads it at start-up and runs only the password commands it lists.

`settings_folder.toml` holds the folder chosen in Settings › Files (`folder`), and, until the
next start has copied the current settings there, the folder to copy them from (`copy_from`).
DbRover writes it as it quits after a switch, and removes it when the user goes back to the
default folder. It is in the data folder so the choice belongs to the machine and never travels
with the settings it points at. A file that can't be read, or a folder that isn't there, means
the default folder for that session (the app says so; `dbrover mcp` stops instead).

## Data sources

- TOML because it is the file I'll most often edit by hand.
- Each entry has a stable `id` (UUID, written once) so renames don't break consoles, history or
  keychain entries.
- Written atomically (write to temp file, then rename) so a crash never truncates it.
- Owned by the engine: loaded once at start-up, rewritten on every save or delete.
- If the file exists but can't be parsed, the engine refuses all changes and the explorer shows
  the error. A broken file is never overwritten with an empty list.
- `color` is optional (one of `red`, `orange`, `yellow`, `green`, `teal`, `blue`, `violet`,
  `pink`) and only written when set, so files from before it existed load unchanged.
- `safety` is optional (`normal`, `careful` or `read-only`) and only written when it isn't
  `normal` (W2). Exports carry it.
- `host`, `port`, `database`, `user`, `path` and `ca_file` may hold `${NAME}` placeholders,
  resolved when connecting; a port with one is written as a string (W1).
- `project` and `mcp_login` are optional too. `mcp_login = { user = "…" }` is the database user
  `dbrover mcp` signs in as instead of the data source's own (see [MCP server](mcp-server.md)).
- Comments and unknown fields are **not** preserved when DbRover rewrites the file (it is written
  with `serde` + `toml`), and the file header says so. Switching to `toml_edit` to edit the
  document in place is a follow-up if hand-editing becomes common.

```toml
[[data_source]]
id = "6f1c0d9e-3b8a-4c3e-9f57-2d1e8a7b5c40"
name = "local"
color = "teal"
driver = "postgres"
host = "localhost"
port = 5432
database = "app"
user = "juan"

# Optional; files written before TLS existed get mode = "prefer".
[data_source.tls]
mode = "verify-full"
ca_file = "/Users/juan/certs/internal-ca.pem"

[[data_source]]
id = "0b6a4f8e-91d2-4e7a-a1c3-5f0e2d9b7c18"
name = "chinook.db"
driver = "sqlite"
path = "/Users/juan/data/chinook.db"
read_only = true
```

## Exports of data sources (W1)

- **File › Export data sources…** writes `[[data_source]]` tables in `data_sources.toml`'s
  format, plus `format_version = 1` and `[[project]]` tables (id, name, slug, `mcp_enabled`,
  MCP overrides). Built by `dbrover_store::write_export` from types that list every field an
  export may carry; a field added to `DataSource` or `Project` fails to compile there until
  someone decides whether it travels. Never a password, token or command.
- Paths beside the exported file are written relative to it, paths under the home folder as
  `~/…`; `read_export` resolves both again. Paths with `${…}` are left alone.
- A newer `format_version` is refused. Ids (and project slugs) must be unique in the file.
- Import matches by id (projects by id, then by slug), previews before writing, and writes
  `projects.toml` then `data_sources.toml`, each once.

## Secrets

- Passwords go to the OS keychain via `keyring`: service `dbrover`, account `<data-source-id>`.
  The engine only reads the keychain for drivers that use passwords (not SQLite), so opening a
  SQLite file never triggers a keychain prompt.
- Behind the `SecretStore` trait, so tests use an in-memory store. A real-keychain round trip is
  an opt-in test: `cargo test -p dbrover-store -- --ignored`.
- A data source's MCP login has an entry of its own, account `<data-source-id>/mcp`, which only
  `dbrover mcp` reads.
- A data source's SSH login has an entry of its own too, account `<data-source-id>/ssh`: the SSH
  password, or an encrypted key's passphrase. A key file's entry is only read once the key turns
  out to be encrypted, so an unencrypted key never triggers a keychain prompt; the agent login
  has none. `SecretStore` takes the kind of secret (`Secret::Database`, `McpLogin`, `Ssh`).
- SSH host keys are trusted through OpenSSH's own `~/.ssh/known_hosts`, read and appended to,
  never rewritten; `EngineConfig::known_hosts` points elsewhere in tests.
- Deleting a data source removes its keychain entries. If that fails, the failure is only
  logged, since a leftover entry is harmless.
- A data source that signs in with a password and has none saved (or whose keychain entry
  can't be read: locked, declined, unavailable) asks for it when it connects, with **Save to
  keychain** ticked. Whatever is typed is also kept in memory until DbRover quits, so it still
  connects when saving fails or is unticked. Never written to disk. `dbrover mcp` can't ask, so
  it connects without one. Saving the data source with a new password forgets the remembered
  one.
- Later: support `~/.pgpass` and `PG*` environment variables for Postgres, which is what other
  tools on the machine already use.

## Keymap

- `keymap.toml`, only if the user writes one (or creates it from View → Keymap…, which never
  replaces an existing file). Format and action ids: [Keymap](features/keymap.md).
- Read by the engine at start-up and on Reload; DbRover never writes to an existing file.

## Settings

- `settings.toml`, written by View → Settings… on every change (atomically), or by hand.
  Missing settings take their defaults; unknown ones make the file invalid, so a typo is
  reported rather than ignored.
- Read by the engine at start-up (after the keymap, before the data sources), after a save
  and on Reload, and sent as `Event::SettingsLoaded`. A file that can't be read leaves the
  app on its last good settings (the defaults at start-up) and the Settings window shows why.
- Rewriting the file drops comments in it.

```toml
[format]
keyword_case = "upper"  # "upper", "lower" or "preserve" (as typed)
indent_width = 4        # spaces per level, 1 to 8

[lint]
disabled = ["select-star-in-insert"]  # lint rule ids, see features/sql-runner.md
ask_database = true                   # connected consoles prepare (never run) statements

[history]                 # both absent by default: the history keeps everything
max_age_days = 365        # statements older than this go
max_per_data_source = 100000  # only the newest this many of each data source stay
```

An id in `lint.disabled` that names no rule is listed in the Settings window, and ignored.

## Query consoles

- One plain `.sql` file per console: `consoles/<data-source-id>/<name>.sql`, readable and
  editable with any editor (while DbRover is closed). Names are `console`, `console_2`, …; any
  other `.sql` file put in the folder shows up as a saved console
  named after the file.
- Owned by the engine (`ConsoleFiles` in `dbrover-store`): read once at start-up, right after the
  data sources, and written atomically (temporary file, then rename) on `SaveConsole`. The UI
  sends a save a second after typing stops, and for every unsaved console on quit; the engine
  handles queued commands before it stops, so the last edit is written.
- A file that can't be read is logged and left alone; its tab is dropped from the layout.
- Closing a console's tab removes its file if nobody typed in it, and keeps it otherwise.
- Deleting a data source leaves its console folder in place: the SQL in it is the user's.

## ER diagrams

- One TOML file per diagram (ER1, [ER diagrams](diagrams.md#saving)):
  `diagrams/<project-slug>/<name>.toml`, or `diagrams/_none/<name>.toml` for a data source in no
  project (`_none` can't be a slug, which has no underscores). The file names its data source by
  id, the schema it was opened on, and each table with where its box sits, in whole points; a
  table without `x` and `y` is placed by the layout. Columns and keys are never saved: they are
  described again when the diagram opens.
- Owned by the engine (`DiagramFiles` in `dbrover-store`): read once at start-up, right after
  the consoles, and written atomically on `SaveDiagram`. The UI sends a save a second after the
  diagram last changed, when its tab closes, and for every unsaved diagram on quit. An unknown
  field makes the file unreadable, so a typo is reported; rewriting it drops comments.
- Listed under the data source's **Diagrams** menu wherever they are under `diagrams/`, by the
  data source they name: moving a data source to another project moves no file.
- A file that can't be read is logged and left alone. Deleting a data source leaves its
  diagrams; **Delete diagram…** in a diagram's tab removes its file.

## `.sql` files

- A console can show a `.sql` file from anywhere instead (**File › Open…**, or **Save as…** on a
  console, whose file in `consoles/` is then removed once the new file is written). Read whole
  as UTF-8 (another encoding is refused, not mangled) and written atomically by the engine
  (`read_sql_file` and `write_sql_file` in `dbrover-store`) on `OpenSqlFile` and `SaveSqlFile`,
  only when the user saves.
- `CheckSqlFiles`, every two seconds and when the window regains focus, compares each open
  file's modification time with the one last read or written, and sends what a changed file
  now holds (or that it is gone).
- Which file each console tab shows, and the ten recent files with the data source each was
  opened on, are kept in `app.ron` under `sql_files_v1`, next to the layout. A console showing
  a file has a `file_<n>` name of its own, never written to `consoles/`.

## Query history

SQLite table, in `history.sqlite`, created on first start. `PRAGMA user_version` is the
schema's version (2 since W4; files from before it are 0 and are migrated on open, see
[History as an audit trail](history-audit.md#the-schema-version-2)):

```sql
CREATE TABLE history (
    id            INTEGER PRIMARY KEY,
    data_source   TEXT NOT NULL,
    executed_at   TEXT NOT NULL,      -- RFC 3339, UTC, always millisecond precision
    sql           TEXT NOT NULL,
    duration_ms   INTEGER,
    rows          INTEGER,
    outcome       TEXT NOT NULL,      -- ok | error | cancelled
    source        TEXT NOT NULL DEFAULT 'app',  -- app | mcp, what dbrover before W4 reads
    origin        TEXT,               -- console | grid | object | import | export | mcp
    detail        TEXT,               -- the MCP client's name, or the imported file
    changes       TEXT                -- a grid edit's row change, JSON
);
CREATE INDEX history_by_source ON history (data_source, executed_at DESC);
CREATE INDEX history_by_time   ON history (executed_at DESC);
CREATE INDEX history_by_origin ON history (coalesce(origin, source), executed_at DESC);
```

- A row without `origin` (written before W4, or by an older DbRover sharing the file) reads as
  `coalesce(origin, source)`: `app` is "DbRover, before it recorded where".
- `executed_at` always has three fraction digits (`2026-10-22T09:15:02.500Z`) so the text sorts
  in time order: with a variable number of digits, `…:02Z` would sort after `…:02.5Z`.
- One row per statement a console runs, written when it finishes; one per statement the grid,
  an object helper or an export sends; one per CSV import, with the `INSERT`'s columns but not
  its rows. `rows` is what the database
  reports (rows changed, or returned once fully read), `duration_ms` the time until the
  statement completed or its first page arrived.
- Secrets never reach the file: `History::record` replaces passwords in DDL (`PASSWORD '…'`,
  `IDENTIFIED BY '…'`, DuckDB's `CREATE SECRET`) and in connection strings (`password=…`,
  `pwd=…`, `user:password@`) with `<redacted>`, for every writer. See
  [History as an audit trail](history-audit.md#w4b--secrets-stay-out).
- Searchable from the History window of a console (one data source or every one, substring
  of the SQL, where it came from, how long ago; newest first, at most 500 rows), each search
  on an index. Results themselves are not stored. **Export history** writes a search, every
  match without the limit, as CSV or JSON (`executed_at`, `data_source`, `data_source_id`,
  `source`, `detail`, `outcome`, `duration_ms`, `rows`, `sql`, `change`).
- Owned by one engine thread from the blocking pool, so writes and searches never block the
  UI and searches see every earlier write. If the database can't be opened, history is
  disabled for the session and the History window shows why.
- Kept as long as `[history]` in `settings.toml` says (`max_age_days`, `max_per_data_source`;
  no limit by default). The app prunes at start-up and when the setting is saved, 2,000 rows
  per transaction so another process's write never waits long; `dbrover mcp` doesn't prune.

## Layout

The `egui_dock` `DockState` is serde-serializable. It is stored under the `dock_layout` key in
eframe's own storage (`app.ron`, written on exit and every 30 seconds) and restored at start-up.
A saved layout that no longer deserializes (for example after a `Tab` variant is removed) is
replaced by the default layout; **View → Reset layout** does the same on demand.

Console tabs refer to their console's file (data source id and name), so they reopen on start.
A tab whose file or data source is gone is dropped from the layout instead of failing to start;
until the engine has read the files, restored tabs show "Loading…". A tab that showed a `.sql`
file is reopened once the engine has read that file, and dropped if it can't be read.

## The previous name's files

`APP_NAME` names the folders, the keychain service and eframe's app id. `PREVIOUS_APP_NAME` is
the name before it, and while the two differ the first start under the new name brings the old
files over (the plan: [Migrating existing installs](releasing.md#migrating-existing-installs)).

- **Folders.** `adopt_previous_install` copies the previous settings folder and, where it is a
  separate one, the window-state folder, each into a `.migrating` staging folder that is then
  renamed into place. The settings folder goes last, and its existence means "done": nothing
  is copied while it exists. `logs/` isn't copied; `history.sqlite` is copied with
  `VACUUM INTO` from a read-only connection, so the copy includes its write-ahead log and the
  old file is never written. A `.migrating.lock` file beside the new folder makes the app and
  `mcp` take turns. The old folders are left as they were, plus `copied-to-<name>.txt`.
- **When it runs.** First thing in `main` and in `mcp::run`, before telemetry, since on macOS
  the log folder is inside the settings folder. If the copy fails, that session uses the old
  folders (eframe's `persistence_path` included) and the next start tries again.
- **Secrets.** `os_keychain()` reads the current service, then the previous one; a secret found
  only under the previous name is saved under the current one, and the old entry is deleted
  once that succeeded. Deleting removes both. With one name, it is the plain keychain.
