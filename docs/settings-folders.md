# Settings folders

DbRover keeps its settings in a different place on each platform, picked by the `directories`
crate: `~/Library/Application Support/dbrover` on macOS, `~/.config/dbrover` on Linux and
`%APPDATA%\dbrover\config` on Windows. People who keep their dotfiles in git, or sync them,
look for `~/.config/dbrover` on every machine, and some want the settings somewhere else
entirely: a synced folder, a repository, a second profile for work.

H4 does two things, decided on 2026-11-01:

1. `~/.config/dbrover` is the settings folder on every platform, and existing installs are
   copied there on their first start.
2. The settings folder can be one the user chooses, in Settings, with a command-line flag or
   with an environment variable, and `dbrover mcp` finds the same one.

## What lives where

Two folders, as today. The **settings folder** holds what a person writes, or would want on
another machine: `data_sources.toml`, `projects.toml`, `settings.toml`, `keymap.toml`,
`actions.toml`, `snippets.toml`, `theme.toml`, `consoles/`, `diagrams/` and `history.sqlite`.
The **data folder** holds what belongs to this machine: `app.ron` (window and layout),
`logs/`, `trusted_commands.toml`, and, new in H4, `settings_folder.toml` (the folder chosen in
Settings, below).

| Platform | Settings folder | Data folder |
| --- | --- | --- |
| macOS | `~/.config/dbrover` | `~/.local/share/dbrover` |
| Linux | `$XDG_CONFIG_HOME/dbrover`, or `~/.config/dbrover` (unchanged) | `$XDG_DATA_HOME/dbrover`, or `~/.local/share/dbrover` (unchanged) |
| Windows | `%USERPROFILE%\.config\dbrover` | `%USERPROFILE%\.local\share\dbrover` |

**The data folder follows the same rule as the settings folder, `~/.local/share/dbrover`,
everywhere.** The alternative was to leave it in the platform's folder
(`~/Library/Application Support/dbrover`, `%APPDATA%\dbrover\data`). On macOS that folder is
today's settings folder too, so after the move it would hold the live `app.ron`, logs and trust
beside a stale copy of every setting, and the note saying "this folder isn't read any more"
would be half true. With both folders moved, the old folder is entirely the old install: it can
be deleted whole, and one sentence describes the layout on every platform. What is given up is
the platform's convention for machine-local data (`~/Library/Application Support`,
`%LOCALAPPDATA%`), which matters less for a tool whose users asked for `~/.config`.

**Windows uses `%USERPROFILE%\.config` and `%USERPROFILE%\.local\share`,** where tools that
came from Unix, such as Git and Starship, look, rather than `%APPDATA%`. The user asked for one
place on every platform, and the Windows user who keeps dotfiles in git keeps them there. These
folders don't roam with a Windows domain profile, and `%APPDATA%\dbrover` did; nobody has asked
for roaming, and the synced-folder choice below is a better way to get it.

**The XDG variables count on Linux only.** On Linux, `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME`
are the platform's own rule, desktop sessions set them for every program, and DbRover honours
them today; keeping them keeps every Linux install where it is. On macOS and Windows they are
a shell's setting: an app started from the Dock, Finder or the Start menu doesn't see them, so
honouring them would put the settings in one folder when DbRover is started from a terminal and
in another when it is started from the Dock. A choice that should hold everywhere goes in
Settings instead.

## Moving existing installs

`previous_install.rs` already copies dbdesk's folders to DbRover's on the first start under
the new name, safely: it takes a lock, copies into a staging folder and renames it into place
(the commit point), reads the history through SQLite's `VACUUM INTO`, leaves the previous
folder as it was apart from a note, and falls back to the previous folders for a session whose
copy failed. H4 reuses all of it, generalised from one previous install to a list, tried in
order:

1. DbRover's platform folders, as `directories` names them (macOS and Windows; on Linux they
   are the new folders, so there is nothing to copy).
2. dbdesk's platform folders, as today, for someone updating from 0.12 or earlier straight to
   this version.

The first whose settings folder exists is copied, and the rest are ignored: a dbdesk folder
beside a DbRover one was copied already, and has its own note. As today, nothing is done when
the new settings folder exists.

**A previous install that kept everything in one folder** (macOS) is split while it is copied:
`app.ron` and `trusted_commands.toml` go to the new data folder, everything else to the new
settings folder, `logs/` nowhere. Today's copy puts the whole folder in the settings folder,
which would put `trusted_commands.toml` beside `actions.toml` in a folder the user may now sync
or share, the one thing that file must never be (see [Trust](#trust-and-passwords)). The data
folder is written first and the settings folder last, so the settings folder's rename stays the
commit point.

The note in the old folder and the window that says where the data was copied from are worded
for either move ("DbRover now keeps its settings in…"), not only for the rename.

## Choosing the folder

The settings folder is, highest first:

1. `--config-dir <folder>` on the command line, for the app and for `dbrover mcp`;
2. `DBROVER_CONFIG_DIR`, the same for every DbRover the environment starts;
3. the folder chosen in **Settings › Files**, written to `settings_folder.toml` in the data
   folder;
4. the default, `~/.config/dbrover`.

The flag and the variable are for what a setting can't do: tests and scripts that must not
touch the real folder, a second profile started from its own shortcut
(`dbrover --config-dir ~/work/dbrover`), and MCP clients started with a folder other than the
app's. A path must be absolute or start with `~/`, which DbRover expands itself, because MCP
clients' `args` and `env` aren't passed through a shell; a relative path would be relative to
wherever the client happens to start the server. The data folder is never overridden: it is
this machine's, whatever settings are in use.

**The choice is stored in the data folder, not the default settings folder.** The task
suggested a pointer file in the default folder. In the data folder instead it is
per-machine, like the trust, so it is never copied, synced or committed with the settings; it
never creates an otherwise unused `~/.config/dbrover`; and it can't point at itself.

```toml
# settings_folder.toml, written by DbRover
folder = "/Users/ada/Dropbox/dbrover"
# Only until the next start has copied it (see "Switching" below).
copy_from = "/Users/ada/.config/dbrover"
```

**Settings › Files** shows the folder in use and where that came from (the default, chosen
here, `--config-dir` or `DBROVER_CONFIG_DIR`), with **Choose…** and **Use the default
folder**. While the flag or the variable sets the folder, both buttons are disabled and say
why: a choice made here would be ignored until DbRover starts without them. Files is a new
category in the Settings tab with this one row for now, and the natural home for "Open the
settings folder" later.

### Switching

**Choose…** opens a folder picker and checks the folder before anything changes:

| The folder | What happens |
| --- | --- |
| Holds DbRover's files (any of the files or folders above) | **Use the settings there.** Nothing is copied or merged, and the current folder is left as it is. This is how a second machine joins a synced folder. |
| Is empty | **Copy my settings there** (the default) or **Start with no settings**. |
| Holds other files | Refused: "choose an empty folder, or one that has DbRover's files". Copying into a folder of unrelated files would mix them with consoles and diagrams. |
| Is the current folder, or inside the current settings or data folder | Refused; copying a folder into itself never ends. |
| Can't be written (checked by creating and removing a file) | Refused, with the reason. |

**Use the default folder** follows the same rules with the default folder, which may hold the
settings from before the switch, or nothing.

Then **Restart DbRover** quits the usual way (unsaved consoles and open transactions are dealt
with first), writes `settings_folder.toml` once the engine has stopped and everything is
written, and starts again through the updater's existing relaunch. The switch takes effect at
the next start because every file the engine uses is opened at start-up from the paths it was
given; swapping them while it runs would mean reloading every store, consoles with unsaved edits
included.

**The copy happens at that next start, not in the running app,** and through the same code as
the migration, with `copy_from` as the previous install and the chosen folder as the new one.
At the next start the previous session has quit and written everything, consoles included; the
copy runs before anything opens the files, takes the lock against `dbrover mcp` starting at
the same moment, and commits with a rename; a failed copy keeps the old folder for that session
and says why, as the migration does. `copy_from` is removed once the copy is done. A crash
after the rename and before that leaves a folder that now holds DbRover's files, so the next
start sees there is nothing to copy and only removes `copy_from`. The empty folder the picker
chose is removed just before the rename, which Windows needs.

### Folder problems at start-up

- **The chosen folder is missing** (a drive or a network share that isn't mounted, a synced
  folder that was moved): the app uses the default folder for that session and says so in a
  window, naming the missing folder; `settings_folder.toml` is kept, so the next start uses
  the folder again once it is back. It isn't created: creating `/Volumes/Backup/dbrover` on
  the startup disk would hide the real one when the drive comes back. `dbrover mcp` stops with
  an error that names the folder instead of serving the default folder's projects, which an
  MCP client would take for the user's.
- **A folder from `--config-dir` or `DBROVER_CONFIG_DIR` that is missing** is created, as the
  default folder is: the user typed it for this start.
- **A folder that can't be written:** saving fails with the usual error for each file, as it
  would in the default folder. Only **Choose…** checks first.
- **A folder that holds another install's files** is used as it is, which is the point of
  choosing one.
- **Two copies of DbRover on one folder** are as safe as the app and `dbrover mcp` already
  are today: every file is written whole and atomically, so a file is never half one copy's
  and half the other's, but the last copy to save a file wins; and the history is SQLite,
  which takes turns between writers. No lock is added. **Two machines on one synced folder**
  are fine one at a time; at the same time, the sync service decides which copy of a file
  wins, and it may copy `history.sqlite` while SQLite is writing to it. The manual says to
  quit DbRover on one machine before using the other.

## MCP

`dbrover mcp` resolves the folder the same way, and runs the same pending copy if it starts
first. A folder chosen in Settings is in the data folder, so an MCP client finds it with the
configuration it already has: no client needs to be set up again after **Choose…**.

**Copy MCP configuration** adds `"--config-dir", "<folder>"` to the server's `args` only when
the app's folder came from `--config-dir` or `DBROVER_CONFIG_DIR`, which the client won't
pass on; otherwise the configuration is what it is today. `dbrover mcp --help` lists the flag.

## Trust and passwords

- `trusted_commands.toml` stays in the data folder, never the settings folder, whatever is
  chosen: a synced, shared or copied settings folder brings its command actions and password
  commands but none of the trust, and each command asks before its first run on every machine,
  as today. The migration's split above keeps it there for macOS installs too.
- `settings_folder.toml` is in the data folder for the same reason.
- Passwords stay in the OS keychain under the service `dbrover`, keyed by data source id, not
  by folder. A folder copied on the same machine keeps its ids, so its data sources find the
  same passwords; on another machine they ask once. Two folders with the same data source
  share its keychain entry, so deleting it in one forgets the password for the other too; the
  manual says so.

## Docs

- The book's [Where DbRover keeps its files](../book/src/files.md): the new table, and a
  section on choosing the folder, the flag and the variable.
- [Coming from dbdesk](../book/src/install.md#coming-from-dbdesk) and a new "Settings moved
  to ~/.config/dbrover" note beside it.
- [Connecting an AI assistant](../book/src/mcp.md): `--config-dir` in a client's `args`.
- [Persistence](persistence.md), "Migrating existing installs" in
  [Releasing](releasing.md), `README.md` and the measurement script's notes.
- The release notes: the move is a breaking change to where saved files are (`feat!`), so the
  release that ships it calls it out first: its release PR adds a short "Your settings moved"
  paragraph to `packaging/release-notes.md` (changing that file builds every package, which
  the release PR does anyway), and the release after it removes the paragraph.

## How it's built

- `dbrover-store`: `AppPaths` stops holding a `ProjectDirs` and holds the two folders, built
  from the home folder (and the XDG variables on Linux), from a previous install's platform
  folders, or from a chosen settings folder. `previous_install` takes a list of candidates and
  routes the data files of a one-folder install. A new `settings_folder` module reads and
  writes `settings_folder.toml`, resolves the four sources into the folder and where it came
  from, and checks a folder for **Choose…**. All of it is pure or works on paths it is given,
  so tests use temporary folders, never the real ones.
- `dbrover-app`: `--config-dir` for the app and `dbrover mcp`, `DBROVER_CONFIG_DIR`, the
  window for a missing folder, the Settings row, and `--config-dir` in Copy MCP configuration.

## Layers

A GitHub stack, each layer with its tests:

- **H4a — The plan:** this document and the roadmap entry.
- **H4b — `~/.config/dbrover` everywhere:** the new defaults, the list of previous installs,
  the one-folder split, and the reworded note and window. Migration tests in the style of
  `previous_install/tests.rs`: each platform's previous layout to the new one, the dbrover
  folder preferred over dbdesk's, trust never in the settings folder, an existing new folder
  left alone.
- **H4c — Where the folder comes from:** `settings_folder.toml`, the resolution order, the
  pending copy through the migration code, and the folder checks, unit-tested in the store.
- **H4d — The flag and the variable:** `--config-dir` and `DBROVER_CONFIG_DIR` in the app and
  `dbrover mcp`, the missing-folder window and error, and Copy MCP configuration. The
  `mcp_subcommand` integration test runs the real binary with each.
- **H4e — Settings › Files:** the row, **Choose…** with its checks, **Use the default
  folder**, and the restart, with kittest tests.
- **H4f — Manual and docs,** the release-notes paragraph, and the demo note.

## Risks

- **A synced `history.sqlite`.** SQLite in a folder that Dropbox or iCloud syncs can be copied
  mid-write. Keeping the history in the data folder would avoid it, but moves it out of the
  folder that Linux users have had it in since M4, and splits "everything I'd want on another
  machine" in two. It stays in the settings folder; the manual warns, and moving it is a
  follow-up if it bites.
- **A second migration so soon after the rename.** macOS users copied dbdesk's folder in 0.13
  and now copy again. Each copy is safe and leaves the old folder whole, at the price of disk
  space and a folder to delete by hand; the note in it says so.
