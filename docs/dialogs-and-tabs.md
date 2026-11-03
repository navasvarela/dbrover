# Dialogs and tabs

Status: **planned** (2026-11-03; the open questions answered the same day). This is the design of D8 in the
[roadmap](roadmap.md#d8--one-style-for-dialogs-and-tabs-for-what-stays-open).

D7 gave dialogs a window's margin and buttons by role, but each dialog still lays itself out:
its own title, its own row of buttons, its own width. Looking at all of them side by side, they
read as thirty small programs rather than one app. Some of them are also popups only because
they were written before the dock could hold anything but consoles: the query history and an
object's definition float over the consoles and hide them, although they are things people keep
open beside their work. Settings moved from a window to a tab in #286 for the same reason, and
Reports and Diagrams were tabs from the start.

So D8 does two things, in this order: one style for every popup that stays a popup, and a dock
tab for each popup that people keep open.

## What's there

There are 31 popups: 24 modal dialogs, all through `widgets::dialog`, and 7 floating
`egui::Window`s.

| Popup | Kind | Title | Footer | Width |
| --- | --- | --- | --- | --- |
| About | modal | logo and `heading` | own row, no primary | content |
| Check for updates | modal | `heading` | own row; primary only with an update | 360 at least |
| Data source (new, edit) | modal | `heading` | own row | 420 |
| Delete data source | modal | `heading` | `form::footer` | content |
| Export data sources | modal | `heading` | own row | 420 |
| Import data sources | modal | `heading` | own row | 560 |
| Export rows | modal | `form::Header` | `form::footer` | 560 |
| Import a CSV file | modal | `heading` | own row | 640 |
| Password prompt | modal | `heading` | own row | content |
| Password command prompt | modal | `heading` | own row | 460 |
| SSH host key | modal | `heading` | own row | 460 |
| Parameters | modal | `heading` | own row | content |
| Hazard (run anyway, apply edits) | modal | `heading` | own row | content |
| Roll back open transactions | modal | `heading` | own row | content |
| Unsaved consoles | modal | `heading` | own row; "Don't save" plain | content |
| Open a SQL file, and its failure | modal | `heading` | own row; failure has no primary | content |
| Project name, delete project | modal | `heading` | own row | content |
| Use another settings folder | modal | `heading` | `form::footer` | 460 at most |
| Settings folder notice | modal | `heading` | own row, no primary | 420 at most |
| New diagram, delete diagram | modal | `heading` | own row | 360 |
| Run a program? | modal | `heading` | own row | content |
| Object dialog's careful-source check | modal | `heading` | own row; Run plain | content |
| Object dialog (new table, drop, …) | window | egui title | own row; Execute plain | 620 |
| History | window | egui title | none | 720 × 520 |
| Definition | window | egui title | none; Copy at the top | 560 × 380 |
| Cell viewer | window | egui title | none; Copy at the top | 520 × 360 |
| Snippets and theme file problems | window | egui title | none; Reload at the top | 560 × 280 |
| Program output, program failure | window | egui title | none | 520 × 360 |

Five things stand out:

- **Titles in three styles.** egui's title bar on windows, a plain `ui.heading` in 22 modals,
  and the form header with its icon badge only in the export dialog.
- **Footers built by hand.** Only three dialogs use `form::footer`; the rest repeat a separator
  and a right-to-left row, with slightly different spacing each time.
- **Roles missing or wrong.** The object dialog's **Execute** is a plain button even for a
  `DROP`; the careful-source **Run** is plain where the same question elsewhere is a red **Run
  anyway**; **Don't save** looks like **Cancel**; About, the folder notice, the SQL file failure
  and Check for updates have no primary action.
- **Sizes as literals.** Widths of 360, 420, 460, 560, 620 and 640 points and list heights
  from 180 to 380, each a constant private to its file.
- **Fields laid out by hand.** The data source and object dialogs build their own grids instead
  of `form.rs`'s cards, so their fields are white on near-white in the light theme, the problem
  D7 noted in [Buttons and menus](buttons-and-menus.md).

## The dialog kit

Every modal is built from the same four parts, so a new dialog can't come out different:

1. **A size.** `widgets::dialog` takes a `DialogSize` and sets the width itself; no dialog sets
   its own width again.

   | `DialogSize` | Width | For |
   | --- | --- | --- |
   | `Confirm` | 420 | a question: delete, roll back, run anyway, unsaved consoles, notices, About |
   | `Form` | 480 | a few fields: passwords, host key, project name, parameters, a new diagram |
   | `Wide` | 560 | a list or a preview: import and export of data sources, export rows, open a file, the data source dialog |
   | `Large` | 680 | a big form: CSV import, the object dialog |

   Lists and previews inside a dialog take one of two heights, `LIST_HEIGHT` (280) or
   `TALL_LIST_HEIGHT` (380), from the same place. The widths are today's, rounded into four
   steps; `Form` grows from 460 to 480 so the data source dialog's 280-point fields and their
   labels fit a card.
2. **A header.** `form::Header` at the top of every modal: an icon in a tinted badge, the title,
   and a line naming what the dialog acts on (the data source, the table, the file). Dialogs
   that destroy something tint the badge with the error colour (`Header::danger`). About keeps
   its centred logo instead of a badge: it is the one dialog that is about DbRover itself.
3. **The body** in `form.rs`'s parts: `form::section` and `form::card` for fields, so fields
   have an outline and a card behind them in both themes. The data source dialog and the object
   dialog move to cards.
4. **A footer.** `form::footer` at the bottom of every modal: a line, then the actions right to
   left, the primary one in the corner. An action that throws away work, such as **Don't
   save**, sits at the footer's left, apart from the others (`form::footer` gets a leading
   side for it), as macOS places it.

And the roles, which D7 introduced, applied without exception:

- **Primary** (`form::primary_button`): the one action the dialog is for. Every dialog has
  exactly one, including a notice whose only action is **OK** or **Close**.
- **Danger** (`form::danger_button`): an action that destroys something or runs what DbRover
  warned about: Delete, Drop, Roll back, Run anyway, Don't save, the object dialog's
  **Execute** when the statement drops or truncates, and the careful-source **Run**. A danger
  action replaces the primary; a dialog has one or the other.
- **Secondary** (`ui.button`): everything else, Cancel included.

Floating windows that stay popups keep egui's title bar, which is how they are moved, and get
the same footer and roles: **Copy** and **Close** at the bottom instead of a Copy button over
the text. They open centred below the menu bar rather than at (16, 16), over it, a D7 follow-up.

## Popups and tabs

The rule: **a tab is for something you keep open beside your work and come back to; a popup is
for a question or a short task that ends.** A tab must also have a key that names it, so the
saved layout can bring it back.

| Popup | Becomes | Why |
| --- | --- | --- |
| History | a tab per data source, restored after a restart | people keep it open while writing statements; it hides the console it inserts into |
| Definition | a tab per object, not restored | read beside a console; several at once are useful, and a window holds one |
| Snippets and theme file problems | sections of the Settings tab's Editor and Appearance pages | the other config files' problems are already Settings pages (Keymap, Custom actions, Exports) |
| Cell viewer | a side panel in the console | tied to the grid's selected cell, which a tab would lose |
| Export rows, CSV import | stay modal while being filled in; the run moves to the status bar | a long run no longer blocks the app, and a short form doesn't become a tab |
| Object dialog | stays a floating window, restyled | a short task that ends; non-modal already, so the explorer stays usable |
| Program output and failure | stay floating windows, restyled | read once and closed |
| Every other modal | stays modal | a question or a short form that must be answered before going on |

### History as a tab

`Tab::History(DataSourceId)`, one per data source, titled "History of *name*". The search box,
filters and the "every data source" scope stay as they are. Its state lives on `DbRoverApp`
beside the reports' and diagrams', keyed the same way.

- **Insert** puts a statement into the console the history was opened from while that console
  is open, otherwise into the data source's most recently focused console, otherwise into a new
  one. The tab doesn't need to remember the console across a restart.
- **Restore:** at start-up `workspace::restore_history` keeps the tabs whose data source still
  exists and sends their search again; the history is in DbRover's own store, so this needs no
  connection. A tab whose data source was deleted is dropped, and deleting a data source closes
  its tab.

### Definition as a tab

`Tab::Definition(DefinitionKey)`, the data source and the object's catalog path, titled with the
object's name. The text is read-only in the console's code editor, with its syntax colours;
the footer offers **Copy** and **Open in console**, which puts the text into a
new console to edit and run (`CREATE OR REPLACE …`).

Why a tab rather than opening the text read-only in a console: a console is a file in the
project folder, so every definition looked at would leave a file behind or need a kind of
console that isn't saved, and a read-only console would show a Run button that can't run.
**Open in console** gives the editable copy when it is wanted.

Not restored: loading a definition needs a live connection, and reconnecting every data source
with an open definition at start-up could ask for passwords before the user did anything.
Definition tabs are left out of the layout when it is saved, so they never need a key that
survives a restart, and reopening one is **Show definition** in the explorer again.

### The config files' problems in Settings

The status bar's link for `snippets.toml` opens the Editor page and the one for `theme.toml` the
Appearance page, each with a section for its file: where it was read from, what it adds, its
problems and **Reload**, in the same `source_line` and `problems` parts the Keymap, Custom
actions and Exports pages use. The window and `CustomFiles::window` go.

### The cell viewer as a side panel

The cell viewer becomes a panel on the right of the console's results, beside the grid, as
DataGrip's value editor is. It shows the selected cell and follows the selection, so moving
through a column of JSON reads each value in turn. The grid's toolbar has a toggle for it, and
**View value** on a cell opens it. It keeps today's views: JSON as a tree or as text, XML
indented, `NULL` in italics, and **Copy**. Each console remembers whether its panel is open while
DbRover runs, and the panel's width is a splitter, like the one between the editor and the
results.

### Export and CSV import run outside their dialog

Both dialogs stay modal while they are filled in: the format, the options, the preview. Once
**Export** or **Import** is clicked the dialog closes and the run goes on in the background, with
its progress and **Cancel** in the status bar, where update notices and config-file problems
already show. When it ends, the status bar says how many rows went where, with a link to reveal
the file for an export, for a few seconds and in the log console for good; a failure stays in the
status bar until clicked. Several runs at once show as one entry that counts them, and a click
lists each with its own **Cancel**.

### The object dialog and program output stay windows

Both stay floating, non-modal windows, restyled: the object dialog's fields in cards, its
**Execute** red when the statements drop or truncate, and both with the footer's **Copy** and
**Close** in place of buttons over the text.

## Answers to the open questions

The plan asked four questions; the user took each leaning on 2026-11-03:

1. **The cell viewer** becomes a side panel in the console, not a tab.
2. **Export and CSV import** keep their modal form, and their runs move to the status bar,
   rather than becoming tabs.
3. **The object dialog** stays a floating window, restyled, rather than a tab.
4. **Program output** stays in floating windows with the footer's **Copy** and **Close**, rather
   than in the log console or a tab per run.

## In layers

One pull request each, stacked:

1. **D8a — Plan:** this page and the roadmap entry.
2. **D8b — The dialog kit:** `DialogSize` and the list heights, `form::Header` (with
   `Header::danger`) and `form::footer` (with its leading side) in every modal, and the roles
   corrected everywhere. The floating windows that stay get the footer and open below the menu
   bar.
3. **D8c — Cards for the data source and object dialogs:** their fields in `form::section` and
   `form::card`, so they read like the Settings tab and the export dialog.
4. **D8d — History as a tab,** keyed by data source and restored after a restart.
5. **D8e — Definitions as tabs,** with **Open in console**.
6. **D8f — The config files' problems in Settings,** and the window removed.
7. **D8g — The cell viewer as a side panel** in the console.
8. **D8h — Export and CSV import run in the background,** with their progress in the status bar.
9. **D8i — Manual:** the pictures and text of what changed.

Tests are kittest tests in the style of the existing ones: each changed dialog keeps the test
that finds its title and clicks its actions; each new tab gets one test that opens it, one that
saves and restores the layout, and one that it closes when its data source goes. The cell
viewer's panel gets a test that it follows the selection, and the background runs one that the
dialog closes while the export goes on and the status bar reports it. No test waits
on a database: history comes from a temporary store, and a definition tab is fed its
`DefinitionLoaded` event directly.

## Decisions

- **Size is a parameter of `widgets::dialog`,** not a constant per file: a width outside the
  four can't be written by accident, and changing a step changes every dialog of that kind.
- **One action per dialog is filled,** primary or danger, never both. Two filled buttons side by
  side make neither stand out.
- **Don't save is danger, at the footer's left.** It throws away edits, so it gets the colour of
  other destructive actions, and its place keeps it away from **Save** under a habitual click.
- **Floating windows keep egui's title bar;** a badge header under a title bar would name the
  window twice.
- **History is per data source,** as it is opened from a console and filtered to its data source
  by default; the "every data source" scope stays a filter inside it, so there is no separate
  global history tab.
- **Definition tabs aren't restored,** because restoring would connect at start-up.
- **Runs report in the status bar, not in a tab or a window:** a run needs a glance and a
  Cancel, not a place of its own.

## Risks

- **Rebases with the reports work.** `app.rs` and `workspace.rs` are shared with RP3; the tab
  layers keep their changes there to the new `Tab` variants, the `open_*` and `restore_*`
  functions and the state fields, and put the rest in their own modules.
- **An older DbRover can't read a newer layout.** A saved layout with a History tab fails to
  load in a version without it, which falls back to the default layout; that is the existing
  rule for `Tab`, and only matters when downgrading.
- **Tests that find a window by its title** change when it becomes a tab; they move to the tab's
  title, which kittest finds the same way.
- **Every manual picture of a dialog changes.** They are rendered again once, in D8i.

## Demo note

Planned. What to try once built: open three dialogs of different kinds (Delete… on a data
source, the data source dialog, Export…): each has the same header with an icon, the same footer
with one filled button in the corner, and Delete is red with a red badge. Open the history from a
console, dock it beside the console, double-click a statement and it lands in the console;
quit, start again, and the history tab is back. **Show definition** on two views in the
explorer, and their definitions open as tabs to put side by side; **Open in console** on one
puts its text in a new console. Break `theme.toml`, and the status bar's link opens the
Appearance page with the problem listed. Click a JSON cell with the value panel open, and move
down the column with the arrow keys: the panel follows. Export a large table, and the dialog
closes while the status bar counts rows; keep working, and **Cancel** it from there.
