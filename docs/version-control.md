# Version control

Status: **proposed on 2026-11-04, planned, not scheduled.** The
[roadmap](roadmap.md#version-control-track) tracks it as VC1 to VC3. It doesn't move CI fixes,
the reports track (RP5 and what it left for later) or W13 schema compare, which come first.

Many database tools say they support Git. This plan asks whether DbRover should, and answers
**only a small piece**: no Git client inside the app, but the files DbRover already keeps made
safe and tidy under Git, a repository's folder of `.sql` files in the explorer, and schema
snapshots that W13 can compare and Git can keep. The reasons follow the evidence.

## The question

DbRover keeps everything a person writes in plain files, which is one of the two reasons it
exists ([Vision](vision.md): "driven by plain config files I can version and tweak"). Vision
also lists **VCS integration** among the non-goals, as part of not chasing DataGrip and
DBeaver feature for feature. So the question isn't whether DbRover's files can live in Git;
they were designed to. It is whether DbRover should do Git's work in its own window (commit,
diff, blame, branches), and what it should change so that files kept in Git behave well.

## What other tools do

Researched on 2026-11-04 in the tools' docs and issue trackers; Reddit couldn't be read, and
some JetBrains forum pages only through search summaries. Vote counts are GitHub 👍 on that day.

| Tool | What it versions | In the app, or files on disk |
| --- | --- | --- |
| **DataGrip** | Project files, and a *DDL data source*: a folder with one `CREATE` file per object, mapped to a live database, updated from it and applied to it through a migration dialog [G1][G2] | In the app, the whole IntelliJ Git client: commit, branches, diffs, merge request review |
| **DBeaver** | A whole project: connections (`data-sources.json`), scripts, diagrams, bookmarks [G5] | In the app, through Eclipse's EGit; Enterprise and Ultimate in the current docs, moved between editions twice in 2026 [G5][G26] |
| **SSMS 21 and 22** | `.sql` script files only; SQL projects (one file per object) in a 22.4 preview [G10][G12] | In the app, Visual Studio's Git UI |
| **Azure Data Studio** (retired 2026-02-28) | Scripts and notebooks in a folder; SQL projects through an extension [G13] | VS Code's generic Source Control view |
| **DbVisualizer Pro** | Scripts in mounted folders; connections aren't mentioned [G14] | In the app: clone, branches, commit and push, diff, log |
| **TablePlus** | Nothing; saved queries are `.sql` files, and a folder of your own can be added to Favorites [G15][G16] | Files on disk |
| **Beekeeper Studio** | Nothing in Git; 6.0 added version history of saved queries inside the app, and an importer for a folder of `.sql` files that skips `.git` [G18][G19] | Neither: queries live in its own SQLite database [G21] |
| **pgAdmin, HeidiSQL, Sequel Ace, Navicat** | Nothing; Navicat's advice is to export the model to SQL and commit that [G22][G23] | — |
| **Redgate, dbForge, Flyway, Atlas, Liquibase, Bytebase, Prisma, Drizzle** | The schema, as one script per object (state) or as a chain of migrations | Add-ins, command lines and CI, where database teams actually keep schemas in Git |

What users ask for and say:

- **The request is a folder of `.sql` files, opened and saved in place, not a Git panel.**
  TablePlus #663 asked to "mount a directory of sql files" to commit them; the answer was a
  symlink, and then **Add Folder to Favorites** [G15]. In #833 (5 👍, 19 comments) a user whose
  SQL all lives in a repository calls the gap a "deal breaker" [G16]. Beekeeper's #5001, opened
  the day this was written, is a customer who reviews SQL in pull requests and pastes it into
  Beekeeper after each merge [G19]. pgAdmin's maintainers answered a 2022 request that it "is
  not a full IDE" [G22]. Vendors answered with folders (TablePlus, DbVisualizer, Beekeeper's
  importer), not with Git.
- **Demand for Git inside a light client is low.** No Git request in TablePlus's tracker has
  more than about 5 👍, DBeaver's "Integration with Git" has 9 [G7], and Beekeeper's has none.
  In the same trackers, syncing connections (19 👍) and databases (26 👍) in TablePlus [G17][G29],
  and folders for connections in Beekeeper (11 👍) [G20], get more.
- **Built-in Git is where the complaints are.** DBeaver's plugin fails to install or update
  after upgrades (#23451 [G8], #39443 [G27]), and Git logins of several projects overwrite each
  other (#12328 [G28]). Its saved passwords are encrypted with a key
  in the open-source code, so a committed `credentials-config.json` is plain text [G9]; #21567
  asks to split shared and per-user project files because per-user state and changing ids
  churn every diff [G6]. On SSMS 21's Git, readers of Brent Ozar's walkthrough prefer GitHub
  Desktop ("Much simpler"), and one found it "not as useful as I was hoping" since the schema
  still needs Redgate to reach Git [G11].
- **Schema as files is hard to get right.** DataGrip's "Database in version control" request
  took eight years (DBE-3852, 2014 to 2022) [G3]; its own tutorial says the migration dialog
  "is still under development" [G2], it can't read `ALTER` scripts (DBE-11658, open, 13
  votes) [G4], and a forum thread reports a fresh dump that still differs from the database it
  came from. Redgate's SQL Source Control is mostly complained about for slowing SSMS [G24].
  Tool-written metadata conflicts in merges: Drizzle's snapshot files (#2488, 52 👍) [G25].
- **What is praised:** DataGrip's Git being simply there, as in any JetBrains IDE; Redgate's
  and dbForge's history per object and drift against a scripts folder; and secrets kept out of
  shared files, as DataGrip's split of `dataSources.xml` and `dataSources.local.xml` with the
  keychain does [G1][G30].

## DbRover's files, and how each fares in Git today

Everything below is in the settings folder (`~/.config/dbrover`, or the one chosen in
**Settings › Files**, see [Settings folders](settings-folders.md)) unless it says otherwise.
Passwords are in the keychain and trust in the data folder, never here.

| File | Written by | In Git today |
| --- | --- | --- |
| `.sql` files anywhere on disk (W6) | You, with **Save** | **Works.** DbRover looks at open files every two seconds and when the window comes back to the front, so a `git checkout` or `git pull` shows up; a console with unsaved changes asks before reloading. Which data source a file opens on is remembered in `app.ron`, on this machine only. |
| `consoles/<data source id>/*.sql` | DbRover, as you type | **Noisy, and unsafe while running.** Saved a second after you stop typing, so every pause is a change; read at start-up only, so a checkout while DbRover runs is overwritten by the next save. Folder names are data source ids. |
| `data_sources.toml`, `projects.toml` | DbRover, on every save | **Unsafe while running, and loses comments.** Read once at start-up and rewritten whole from memory, so a `git pull` that changes them while DbRover runs is silently undone by the next save of any data source or project. Comments are dropped (the file's header says so). No secrets: passwords are in the keychain, and password commands are trusted per machine. |
| `settings.toml` | DbRover, from **View → Settings…** | The same as above: read at start-up and on **Reload**, rewritten whole on save. |
| `diagrams/<project>/*.toml` (ER1) | DbRover, when a diagram changes | **Designed for it:** places in whole points, no columns or keys, so diffs are small ([ER diagrams](diagrams.md#decisions)). Rewritten from memory, so the same lost-update risk applies to a diagram open in a tab. |
| `keymap.toml`, `actions.toml`, `snippets.toml`, `theme.toml`, `exports.toml` | You | **Works.** DbRover never rewrites them (only **Create** writes a new one), and **Reload** reads them again. Command actions and export programs ask before their first run on each machine, because trust lives in the data folder, so cloning someone's settings can't run a program unasked. |
| `reports/*.toml` (RP) | You | **Works**, and was chosen for it: one `reports/` folder, "one place to sync, keep in git or point RP6's command line at" ([Reports](reporting.md#rp1s-decisions)). But a run doesn't record which version of the report ran (below). |
| `history.sqlite` | DbRover, on every statement | **Shouldn't be committed.** Binary, changes on every statement (with `-wal` and `-shm` files beside it while open), and holds the text of every statement and the old values of grid edits. Nothing in the folder says so. |
| Data source exports (W1) | **File › Export data sources…** | **Works**, and is the documented way to share a team's databases in a repository ([Sharing](../book/src/sharing.md)): paths beside the file are written relative to it, and import matches by id. |

So the hand-written half already works with plain Git, and was designed to. The half DbRover
writes has two problems: it overwrites changes Git makes while it runs, and `history.sqlite`
sits beside the settings with nothing to keep it out of a commit.

## Where in-app support would add value

Each candidate from the question, weighed for a single-user desktop app whose users already
have Git and an editor:

| Candidate | Value | Verdict |
| --- | --- | --- |
| **Files that don't fight Git:** no lost updates, comments and order kept, a `.gitignore` for the history | Every user who keeps the settings folder in Git or a synced folder hits the lost update sooner or later, and it loses data silently | **Yes, VC1.** Small, and it fixes a real defect rather than adding a feature. |
| **A folder of `.sql` files in the explorer:** a repository's `queries/`, browsed, opened and saved in place | The request other tools' users make most (TablePlus #663 and #833, Beekeeper #5001), and the one their makers answered. W6 opens one file at a time; a team's folder of fifty needs a tree | **Yes, VC2.** W6 already opens, saves and reloads files after a checkout; the tree is what's missing. |
| **A report's run tied to the report's version** | An accountant asked "which version of the report wrote this file?" can answer it. RP's use case is accounting, where that question comes up | **Yes, in VC1.** Record the report file's Git blob id with each run: computable without Git, found with `git log --find-object`. |
| **Schema snapshots compared over time** | "What changed in production's schema since the last release?" is W13's question with one side in the past. A snapshot file in Git makes every past schema comparable | **Yes, VC3, with W13.** A snapshot is a file; Git keeps its history; W13 compares a live schema with a snapshot. No Git code needed. |
| **Diff of a saved query against the last commit** | Useful, but the same file is usually open in an editor that already shows it | **Later, if asked.** Reading a blob at `HEAD` needs a Git library or running `git`; neither is worth it for a gutter mark. |
| **Blame** | Low for a single user's queries; a team's history is better read in their Git host | **No.** |
| **Commit, push, pull and branches from the app** | Saves a switch to a terminal or Git client | **No.** A Git client in egui means credentials, SSH and signing, conflicts and a whole UI, to do worse what every editor and Git client does. DataGrip gets it from the IntelliJ platform; DbRover would have to build it. |
| **Branch-aware projects** (data sources or settings per branch) | A branch per environment is rare; `${NAME}` placeholders and `--config-dir` already give per-environment data sources | **No.** |

## Recommendation

**Only a small piece.** Keep "VCS integration" a non-goal, reworded so it says what that means:
no Git client in the app. Then build three milestones, none of which links a Git library:

1. **VC1 — Files that live well in Git (M).** The defects and gaps above: lost updates,
   comments, the history's `.gitignore`, and a report's version on each run.
2. **VC2 — A folder of queries (S–M).** Folders of `.sql` files you add, as a tree in the
   explorer, opened and saved in place through W6.
3. **VC3 — Schema snapshots (S–M), with W13.** A schema saved as a diffable file, and W13
   comparing a live schema with one. Built with W13 on 2026-11-05; see
   [Comparing schemas](schema-compare.md#snapshot-files-vc3).

All three are planned, not scheduled, after CI fixes, the reports track and W13. VC1 and VC2
don't depend on each other or on W13. VC1's lost-update layer (VC1b) is a bug fix that may be
worth pulling forward on its own if someone hits it first.

What the evidence says, in short: the people who want Git in a database client want their
repository's SQL files opened and saved in place, and bring their own Git; Git built into a
light client draws few votes and most of the complaints (DBeaver's plugin; SSMS 21's readers
preferring GitHub Desktop); the database-aware part no Git tool gives is comparing a live schema with
files, which is where W13 is going anyway; and the tools that put schemas in files have found
it slow and imprecise, so a snapshot stays something W13 compares, never something DbRover
applies. DbRover already keeps secrets in the keychain and trust per machine, which is what
DBeaver's users ask it for.

## VC1 — Files that live well in Git

### Noticing changes made outside DbRover (VC1b)

Every file DbRover rewrites remembers what it read: the modification time and a hash of the
bytes. Before writing, it checks the file on disk again.

- **Unchanged:** written as today, atomically.
- **Changed, and the change is per entry** (`data_sources.toml`, `projects.toml`, keyed by id):
  the file is read again, the one entry being saved or deleted is applied on top of what is
  there now, and the result is written. The explorer reloads the list, and the log console
  says *data_sources.toml changed on disk; reloaded before saving*. Another machine's or
  branch's new data source survives your edit of a different one.
- **Changed, and the file is one document** (`settings.toml`, a diagram, a console in the
  settings folder): nothing is written. The setting page, diagram or console says the file
  changed on disk, with **Reload** and **Keep mine**, as W6's `.sql` files already do; **Keep
  mine** writes over it.
- **Changed and can't be read** (a merge conflict's markers, a typo): nothing is written, and
  the error is shown as a broken file is at start-up. A broken file is never overwritten.
- **Noticing without a save:** the same check runs when the window comes back to the front, as
  for `.sql` files, so after a `git pull` in a terminal the explorer shows the pulled data
  sources without a restart, and a console in the settings folder shows its pulled text.

The book's "edit it only while DbRover is closed" then becomes "DbRover picks up changes when
its window comes back to the front".

Made precise while building it (VC1b):

- **The version is the bytes' Git blob id alone,** with no modification time: the bytes are
  read before every check anyway, and a time adds nothing a hash doesn't. The same id names a
  report's version in VC1e.
- **Data sources and projects:** each change is applied to the file as it is read just before
  writing, whether it changed or not. Entries another program added, changed or removed reach
  the app as the events of the same changes made in it (a removed data source is disconnected,
  its keychain entries kept). A file that breaks while DbRover runs keeps the entries in
  memory, is logged, and refuses saves until fixed; one broken at start-up is read again when
  the window comes to the front, so fixing it needs no restart.
- **Documents:** the engine holds the version of each settings file, console and diagram the
  app last saw, and after reporting a change holds every save of it until the app answers
  (**Reload** shows the file's, **Keep mine** writes over exactly the version shown). Saves
  sent before the app heard of the change are dropped rather than written over it. A change
  found when the window comes to the front is followed silently where there is nothing unsaved
  (a console or diagram whose edits are all written; the settings, which save on each click);
  a file deleted since is no change, as there is nothing on disk to lose.
- **Consoles and diagrams without a tab** take the file's text as it is now, for when they
  are opened again. New files that appear in `consoles/` or `diagrams/` show up at the next
  start, as before.

### Keeping comments and order (VC1c)

`data_sources.toml`, `projects.toml` and `settings.toml` are written with `toml_edit`, which is
already in the build, editing the document in place: a save changes only the entry or setting
that changed, keeps comments, blank lines and the order you chose, and adds new entries at the
end. A diff then shows the one data source you edited. This was already a follow-up in
[Persistence](persistence.md#data-sources) "if hand-editing becomes common"; keeping the files
in Git is that case. Unknown keys are still refused when reading, as today.

### Keeping the history out of commits (VC1d)

DbRover writes a `.gitignore` in the settings folder when it creates the folder, or when a
folder is chosen in **Settings › Files**, if there isn't one there already:

```gitignore
# Written by DbRover. The query history is this machine's, holds the text of every
# statement, and changes with each one; don't commit it.
history.sqlite
history.sqlite-*
# Consoles are saved as you type. Uncomment to keep them out of commits too.
# consoles/
```

It never changes a `.gitignore` that exists, and a folder that isn't in Git is unaffected by
one. **Settings › Files** shows whether the folder is in a Git repository (a `.git` in it or a
folder above it) and, when the folder has no `.gitignore`, offers **Add a .gitignore**. It
doesn't judge one you wrote.

### A report's run names the report's version (VC1e)

Each run in `report_runs` records the report file's Git blob id: SHA-1 over `blob <length>\0`
and the file's bytes, which is what Git itself would call that version of the file. It needs
no Git: the `sha1` crate is already in the build. The report's tab shows it shortened beside
each run, and **Copy** puts `git log --find-object=<id> -- reports/<name>.toml` on the
clipboard, which lists the commits that had exactly that file. The same id goes in the
command line's `--json` events and in the run's log, so a CI job's output names it too. A
report that changed between two runs shows a different id, which the run comparison marks.

This is a schema migration of the history (one nullable column); runs recorded before it show
no id.

### The manual (VC1f)

A section of [Where DbRover keeps its files](../book/src/files.md), **Keeping your settings in
Git**: what to commit, what the `.gitignore` leaves out and why, that passwords stay in each
machine's keychain and trust in its data folder (so a cloned settings folder asks before
running any command), and that DbRover picks up a `git pull` when its window comes back to the
front. A short recipe for a team's repository: `queries/*.sql`, `reports/`, an exported data
sources file, and `dbrover report run` in CI. And this roadmap's demo note.

### Layers

Each layer is a pull request of one stack, and each works on its own:

1. **VC1a — The plan:** this design, the roadmap's entry and Vision's reworded non-goal.
2. **VC1b — Noticing changes made outside DbRover:** the read stamp in `dbrover-store`, the
   check before every rewrite, the per-entry merge for data sources and projects, **Reload**
   and **Keep mine** for documents, and the check when the window comes back to the front.
   Store tests for each case (unchanged, changed per entry, changed document, unreadable), and
   a kittest test that edits `data_sources.toml` behind the app's back and saves another data
   source.
3. **VC1c — Comments and order kept:** `toml_edit` for the three files; tests that a save keeps
   a comment and the order, and changes one entry's lines only.
4. **VC1d — The `.gitignore`:** written when the folder is created or chosen, never replaced;
   the Files page's note and button.
5. **VC1e — Report versions:** the blob id, the history's new column, the tab, `--json` and the
   log.
6. **VC1f — The manual:** the section above and the demo note.

**Exit:** with the settings folder in a Git repository and DbRover running, `git pull` brings a
new data source and a changed one; the explorer shows both once the window is in front, saving
a third keeps all three and the comment above the first, and `git diff` shows only the third's
lines. `git status` doesn't list `history.sqlite`. A report run twice with an edit between
shows two blob ids, and `git log --find-object` with the first lists the commit before the edit.

## VC2 — A folder of queries

W6 opens one `.sql` file at a time, from **File › Open…**, recent files or a drop. A team's
repository holds a folder of them. VC2 shows such folders in the explorer.

- **Adding one:** **File › Add query folder…**, or right-click the explorer's **Query folders**
  section. A folder is listed by its name, with its path on hover; **Remove from list** forgets
  it and touches no file.
- **The tree:** the folder's `.sql` files, and its subfolders that hold any, sorted by name.
  Hidden folders (`.git`, `.idea`) aren't entered. Files Git ignores aren't listed: the
  `ignore` crate (ripgrep's) reads `.gitignore` files as Git does, inside a repository only
  and up to its top. Outside one they mean nothing, as for Git: with no repository to stop
  at, every `.gitignore` up to the root of the disk would count. It is a new dependency, pure
  Rust and small, chosen over `walkdir` (already in the build) plus a hand-written reader of
  `.gitignore` rules, which are subtle. The tree is read again when the window comes back to
  the front, so a checkout that adds or removes files shows.
- **Opening a file** opens it as **File › Open…** does: the same console, the same data source
  question the first time, the same save and reload. Nothing new about files themselves.
- **New query here** on a folder asks for a name and creates an empty `.sql` file there, on
  the focused console's data source: the console last focused, since right-clicking the
  explorer focuses the explorer. With none, it opens as **File › Open…** would.
- **Where the list lives:** `settings.toml`'s `[explorer] query_folders`, with paths under
  the home folder written as `~/…`, as data source exports write them, so a synced settings folder brings the list along
  and a folder missing on this machine is shown dimmed rather than dropped.

**Exit:** add a clone of a repository with `queries/sales.sql` and `queries/admin/users.sql`;
both are in the tree and `.git` isn't. Open `sales.sql`, edit and save it, and `git diff` shows
the edit. `git checkout` a branch that adds `queries/refunds.sql`; it is in the tree once the
window is in front, and an open file the branch changed shows its new text.

### Layers

1. **VC2a — The folder list and the tree:** the setting, the explorer section, reading folders
   in the engine (off the UI thread, since a repository may be large), hidden and ignored
   entries left out, refresh when the window is in front.
2. **VC2b — Opening and creating:** opening through W6's path, **New query here**, a missing
   folder dimmed; kittest tests on a temporary folder with a `.gitignore`.
3. **VC2c — Manual:** the explorer and files chapters, and the demo note.

## VC3 — Schema snapshots

W13 compares two live schemas. VC3 lets one side be a file: **Save schema snapshot…** on a
schema writes what W13 compares (tables, columns, keys, indexes, views) to a file you choose,
and W13 compares a live schema with a snapshot as it compares two live ones. Git supplies the
past: commit a snapshot at each release, and comparing production with last release's file is
comparing it with the file at that commit, checked out or saved with `git show`.

- **Deterministic:** objects sorted by name, one block per object, nothing that changes
  between two snapshots of the same schema (no timestamps, no server version, no object ids),
  so a snapshot taken twice is byte for byte the same and `git diff` shows only what changed.
- **Readable on its own:** a diff in a pull request reads without DbRover.
- **From the command line too:** `dbrover schema snapshot <data source> <schema> -o <file>`,
  so a CI job can commit one after every migration.

- **Built from what W13 compares:** the snapshot is written from the same descriptions W13
  reads from a live schema, not from a dump of DDL, so a snapshot compared with the schema it
  was just taken from is empty by construction. DataGrip's DDL dumps that still differ from
  their source [G2] are the failure this avoids.

The format was decided when W13 started: TOML, one column per line, from DbRover's own
writer ([Comparing schemas](schema-compare.md#the-format)).

### Layers

1. **VC3a — Writing a snapshot:** the file, **Save schema snapshot…**, and
   `dbrover schema snapshot`; tests that two snapshots of an unchanged schema are identical.
2. **VC3b — Comparing with a snapshot:** W13's comparison with a file on one side; a test that
   a fresh snapshot compares empty with its schema.
3. **VC3c — Manual,** and the demo note.

**Exit:** a snapshot of the PostgreSQL
test server's `public` taken twice is identical; after adding a column and an index, comparing
the live schema with the first snapshot lists both, and `git diff` of a new snapshot shows the
same two.

## Decisions

| Question | Options | Proposed | Why |
| --- | --- | --- | --- |
| A Git client in the app | (a) None. (b) Status and diff only, read with `gix`. (c) Commit, push and branches too | **(a) None** | Every user of the feature already has Git and an editor; (b) and (c) add a large dependency and a UI to maintain for less than they do. Vision's non-goal stays, reworded. |
| What to do when a file changed on disk before a save | (a) Overwrite, as today. (b) Refuse and ask every time. (c) Merge per entry where entries have ids, ask for whole documents | **(c)** | (a) loses data silently; (b) asks about every pull, even when the change is to another entry. Ids already make entries independent. |
| How a change is noticed | (a) A file watcher (`notify`). (b) Checking when saving and when the window comes to the front, as W6 does | **(b)** | One mechanism for every file, already proven by W6, no new dependency or thread, and Git changes files while you are in another window. |
| Keeping the history out of Git | (a) Move `history.sqlite` to the data folder. (b) Write a `.gitignore`. (c) Only the manual | **(b)**, with (c) | H4 put the history in the settings folder on purpose, to travel with the settings in a synced folder; (a) would undo that and need a migration. (b) helps exactly the Git users and nobody else. |
| How a run names its report's version | (a) The Git commit, by running `git`. (b) A SHA-256 of the file. (c) The file's Git blob id | **(c)** | (a) needs Git and a clean tree, and fails outside a repository. (b) works everywhere but Git can't look it up. (c) works everywhere and Git can. |
| A snapshot's format | (a) DDL, as the object helpers write it. (b) Structured TOML, one table per object. (c) Both | **(b)**, decided at W13's start in [Comparing schemas](schema-compare.md#the-format) | W13 compares structure, and reading DDL back needs a parser per dialect; TOML reads well in a diff. (a) can be run, which a snapshot shouldn't invite. |
| Where the list of query folders lives | (a) `settings.toml`. (b) The data folder, per machine. (c) A file in each repository | **(a)**, paths as `~/…` | A synced settings folder brings the list, and a clone at the same place under the home folder just works; a folder missing on one machine is shown dimmed. (c) would mean DbRover writing into your repositories. |
| Which data source a `.sql` file in a repository opens on | (a) Remembered per machine, as today. (b) A comment in the file. (c) A file beside it | **(a)**, revisited if asked | The first open asks once; a magic comment is one more format to define for a choice made once. |

## Risks

- **Merging per entry hides a conflict.** Two machines change the same data source: the save
  writes yours over theirs for that entry. That is the same as today for that entry, and every
  other entry survives. The log line names the file, so it can be checked with `git diff`.
- **A pulled file with conflict markers.** It doesn't parse, so DbRover refuses to write it and
  shows the error, as for any broken file; it never overwrites it.
- **Modification times that lie.** Some synced folders and network drives keep the time when
  the content changed. The hash is checked as well as the time, so a changed file is noticed
  whatever its time says, at the cost of reading a small file before each save.
- **`toml_edit` and the files' existing format.** The first save after VC1c may reformat a file
  once (quoting, spacing). The release notes say so; later saves change only the lines that
  changed.
- **A `.gitignore` nobody wanted.** It is a small, commented file in DbRover's own folder,
  written once and never touched again; deleting it is respected.
- **A large repository in the tree.** Reading a folder of thousands of files would stall a
  frame; the engine reads it, only `.sql` files are kept, and ignored folders such as
  `node_modules` and `target` are never entered.
- **Expectations.** "Git support" in other tools' feature lists may lead people to ask for a
  commit button. The manual and the comparison page say plainly that DbRover works with your
  Git rather than replacing it.

## What stays out

- Commit, push, pull, fetch, branches, merges, blame and history views inside DbRover.
- A Git library in the build (`git2`, `gix`), and running `git` from the app.
- Projects or data sources that switch with the branch.
- Syncing settings through a Git remote, as some tools do; a synced folder or a repository you
  pull yourself is the way, with **Settings › Files**.
- Applying a snapshot to a database, as DataGrip's DDL data sources do. A snapshot is one side
  of a comparison; W13's script, read and run by hand, is the only way a change reaches a
  database.
- Migrations as a feature (Flyway, Liquibase, Atlas, sqitch): DbRover runs the SQL they write,
  and W13's script is opened in a console to read and run by hand.
- Versioned data (Dolt-style): a database that versions its own rows is a database, and would
  come as a driver, not as this track.

## Sources

[G1]: https://www.jetbrains.com/help/datagrip/2026.2/ddl-mappings.html
[G2]: https://www.jetbrains.com/help/datagrip/databases-in-the-version-control-system.html
[G3]: https://youtrack.jetbrains.com/issue/DBE-3852
[G4]: https://youtrack.jetbrains.com/issue/DBE-11658
[G5]: https://dbeaver.com/docs/dbeaver/Project-team-work/
[G6]: https://github.com/dbeaver/dbeaver/issues/21567
[G7]: https://github.com/dbeaver/dbeaver/issues/2427
[G8]: https://github.com/dbeaver/dbeaver/issues/23451
[G9]: https://github.com/Tatsh/dbeaver-creds
[G10]: https://learn.microsoft.com/en-us/ssms/release-notes-21
[G11]: https://www.brentozar.com/archive/2024/12/how-to-use-github-in-ssms-v21/
[G12]: https://techcommunity.microsoft.com/blog/azuresqlblog/database-devops-preview-in-ssms-22-4-1/4503858
[G13]: https://learn.microsoft.com/en-us/sql/azure-data-studio/source-control
[G14]: https://www.dbvis.com/docs/ug/working-with-git/
[G15]: https://github.com/TablePlus/TablePlus/issues/663
[G16]: https://github.com/TablePlus/TablePlus/issues/833
[G17]: https://github.com/TablePlus/TablePlus/issues/193
[G18]: https://www.beekeeperstudio.io/blog/release-6.0
[G19]: https://github.com/beekeeper-studio/beekeeper-studio/issues/5001
[G20]: https://github.com/beekeeper-studio/beekeeper-studio/issues/799
[G21]: https://docs.beekeeperstudio.io/user_guide/sql_editor/saving_queries/
[G22]: https://www.postgresql.org/message-id/CACspNH1BKhxDrA%2BAYMJnmYT7U5j%3DvKWvHJVtgMU581Ja7NsdGw%40mail.gmail.com
[G23]: https://www.navicat.com/company/aboutus/blog/3968-version-controlling-your-database-schema-with-git-and-navicat
[G24]: https://forum.red-gate.com/discussion/88157/sql-source-control-slows-downs-ssms
[G25]: https://github.com/drizzle-team/drizzle-orm/issues/2488
[G26]: https://github.com/dbeaver/dbeaver/pull/42289
[G27]: https://github.com/dbeaver/dbeaver/issues/39443
[G28]: https://github.com/dbeaver/dbeaver/issues/12328
[G29]: https://github.com/TablePlus/TablePlus/issues/1324
[G30]: https://www.red-gate.com/hub/product-learning/sql-compare/automating-database-deployments-to-and-from-source-control-using-sql-compare-and-powershell/

The full notes, with every source and what couldn't be confirmed, were kept outside the
repository with the research session's other notes.
