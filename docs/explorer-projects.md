# Data sources into projects

Status: **built** (2026-11-20). This is the design of D9 in the
[roadmap](roadmap.md#d9--data-sources-into-projects-in-one-step).

P0 gave the explorer projects: data sources grouped under foldable headers, with everything else
under a weak **No project**. Putting a data source in a project works, but only one way round —
from the data source. There is no way to say "a new one, in here", and no way to say "that one,
in there". Both end up as the same two-step detour:

- **A new data source in a project** is created from the toolbar's `+`, lands in **No project**,
  and is then moved with **Move to project ›** on its own menu. The project was on screen, and
  often right-clicked a moment earlier, when the work started.
- **Moving an existing data source** means finding it, opening its context menu, opening the
  **Move to project ›** submenu, and picking the project by name from a list — while both the
  data source and the project header are a few points apart on screen.

D9 adds the two gestures that start from where the user is already pointing, and changes nothing
underneath: `projects.toml`, the optional `project` id on each data source, and
`ExplorerAction::MoveToProject` all stay as P0 left them.

## Why this is a D item

The D track is where the explorer's interaction work has lived (D5 the macOS look, D7 the room
buttons and menus take, D8 one style for dialogs and tabs). D9 is of that kind: no new data, no
new file format, no MCP behaviour, and nothing an MCP client can see. It reuses P0's model
whole — the drag layer's only outcome is the `MoveToProject` action the **Move to project ›**
menu already pushes. Putting it in the MCP track would file an explorer affordance under a track
about serving assistants.

## New data source in a project

A project header's context menu grows a **New data source ›** submenu, the same one the toolbar's
`+` opens: an item per engine people run themselves, then a submenu per cloud vendor. Picking an
engine opens the data source dialog as the toolbar does, with one difference — the data source is
created in the project whose header was right-clicked.

It goes at the **top** of the header's menu, above a separator and the existing **Rename…**,
**Project settings…**, **Copy MCP configuration** and **Delete…**. Creating is the item people
reach for most and the only one that is not about the project itself; it also puts the menu's two
"new" items, **New data source ›** and **New project…**, at its two ends rather than side by side.

**No project**'s header gets the same item, where it means what the toolbar's `+` means. Today
that header has only **New project…** on it; with **New data source ›** the two headers read the
same, and "add one here" works from the group that is in front of the user either way.

### How the project reaches the dialog

`ExplorerAction::New` and `NewService` carry no project today, which is the whole reason a new
data source lands in No project. Both become struct variants with a `project: Option<ProjectId>`:

```rust
New { driver: DriverKind, project: Option<ProjectId> },
NewService { service: ManagedService, project: Option<ProjectId> },
```

so every call site has to say which project it means, and the compiler finds them all —
`new_data_source_menu` takes the project it is building for, and the Welcome tab, which shows the
same menu with no explorer in sight, passes `None`.

Nothing else changes. `DataSourceForm` already has a `project` field, carried through the dialog
untouched so that saving an edit keeps the data source in its project; `DataSourceDialog::create`
sets it from the action, `to_data_source` writes it out as it already does, and the engine saves
the new data source into the project without a new command.

The dialog itself says nothing about the project, and gets no field for it. The user just
right-clicked the project, the data source appears under that header the moment it is saved, and
**Move to project ›** is there if it was the wrong one — a field in the dialog would be a second
place to change a data source's project, and the one place that could disagree with the header
the menu was opened on.

The item reads **New data source ›**, but its accessible name is "New data source in
*&lt;group&gt;*". The toolbar's `+` and the Welcome tab's card are both called **New data
source**, so the group is what tells the three apart for a screen reader — and for a UI test,
which otherwise cannot say which of them it clicked.

### The `+` menu stays as it is

The toolbar's `+` does **not** grow a project picker. The menu is already two levels deep for
cloud engines, and a project level would multiply it by every project; the Welcome tab shows the
same menu where no project is on screen at all; and the dialog has no project field to agree
with. The header menu needs no picker because the project is the thing the user right-clicked —
the target is unambiguous and needs no naming. **Move to project ›** remains the way to change
a data source's project after the fact, and now the drag does too.

## Dragging a data source onto a header

Dragging a data source's row onto a project header moves it into that project. Dropping it on
**No project** takes it out of its project, which is the one move that has no header of its own
to aim at otherwise.

| Decision | What D9 does | Why |
| --- | --- | --- |
| What can be dragged | A data source's own row, by its name | The only thing that has a project. Catalog nodes (a schema, a table) keep plain click behaviour, so there is nothing to mistake for a move. |
| Collapsed or expanded | Both | The row is the handle, and it is there either way; an expanded source moves with its tree, which stays expanded. Refusing the gesture on an expanded source would be a rule with nothing behind it. |
| What it looks like while dragging | The source's name is drawn in a pill just below and right of the cursor, over the panel; its row stays as it is | Shows what is in flight without hiding the cursor or the header it is aimed at, and a drag let go over nothing leaves no trace. |
| Valid target hovered | The header row fills with the accent at low opacity and takes a 1-point accent outline, the full width of the panel | The same row-wide fill the selected data source uses, in the accent rather than the selection colour, so "drop here" and "selected" don't look alike. |
| The source's own group | No highlight, and dropping does nothing | A move to where it already is. **Move to project ›** already shows the current project checked and does nothing when it is picked; the drag says the same thing by staying quiet. |
| Any other target | Not a drop zone: no highlight, and a release over it moves nothing | A data source row, a table, **Reports**, **Query folders** — nothing claims the payload, so the drag ends where it started. There is no error to report: the user pointed at something that isn't a project. |
| A list taller than the panel | Dragging into the top or bottom 24 points of the explorer scrolls it, faster the closer to the edge | Without it a project folded out of sight is unreachable by drag, and the gesture would be a dead end exactly when the list is long enough for the gesture to be worth having. |
| Dropping outside the explorer | Nothing | egui drops the payload when the button is released over anything that doesn't read it. |

### How it is built

egui 0.36 has the whole mechanism; D9 adds the gesture, not a drag framework.

- The name is one widget sensing `Sense::click_and_drag()`, with the payload set through
  `Response::dnd_set_drag_payload` on the frame the drag starts: the dragged source's id and the
  group it is shown in.

  Not `Ui::dnd_drag_source`, which was the first plan. That helper lays a separate drag-sense
  widget over the body it wraps, and the row's **secondary** click went to that widget instead of
  the label underneath — so every data source's context menu stopped opening, and 27 app tests
  failed on a missing **Move to project** or **New query console**. One widget that senses both
  has no overlay to lose the click to, and egui's own drag threshold still decides which gesture
  a press becomes. The ghost is painted by hand for the same reason: it is what the helper was
  wanted for.
- A header's drop target is the full-width row rect, taken with `Ui::interact` and
  `Sense::hover()`. `Response::dnd_hover_payload` drives the highlight and
  `Response::dnd_release_payload` the move, both of which test `contains_pointer` rather than
  `hovered`, which is false while something is being dragged.
- A release pushes the `ExplorerAction::MoveToProject(id, target)` that already exists —
  `Some(project)` for a project header, `None` for **No project** — unless the target is the
  source's current group, where nothing is pushed.
- Auto-scroll runs inside the explorer's `ScrollArea`: while a dragged data source's payload is
  in flight (`DragAndDrop::has_payload_of_type`) and the pointer is inside the edge band,
  `Ui::scroll_with_delta` moves the view and the frame asks for a repaint, so the scroll
  continues while the pointer is held still.

## What stays the same

- `projects.toml`, the `project` id on each data source, and every engine command.
- **Move to project ›** on a data source's menu, the only way to move one without a mouse drag,
  and the way to move one to a project that is folded away or filtered out.
- The toolbar's `+` and the Welcome tab's **New data source**, which create in no project.
- Deleting a project still moves its data sources to No project and deletes none.

## In layers

One pull request each, stacked:

1. **D9a — Plan:** this page and the roadmap entry.
2. **D9b — New data source in a project:** `project` on the `New` and `NewService` actions,
   `new_data_source_menu` built for a project, the item on both headers' menus, and the dialog
   creating into it. Tested through the explorer's actions and a kittest run of the whole flow
   from the header to a saved data source.
3. **D9c — Drag onto a header:** the drag source, the drop targets, the highlight, the
   current-group rule and auto-scroll, with the drag test pattern below.
4. **D9d — Manual:** the explorer and data source chapters, with pictures.

## Testing the drag

No egui_kittest test in DbRover performs a drag yet, so D9c writes that pattern. egui_kittest
0.36 has the three primitives it needs — `hover_at`, `drag_at` and `drop_at` — and they are bare
event pushes that step no frames of their own, so the test steps between them:

```rust
harness.hover_at(source);   // the pointer arrives on the row
harness.step();
harness.drag_at(source);    // press
harness.step();
harness.hover_at(middle);   // past egui's drag threshold
harness.step();
harness.hover_at(target);   // over the header
harness.step();
harness.drop_at(target);    // release
harness.run();
```

Two things the pattern has to respect: egui starts a drag only once the pointer has moved a few
points from the press, so a press and release at one position is a click and not a drag; and
`drop_at` also fires `PointerGone`, which clears the hover state, so a snapshot of the highlight
has to be taken before the release. Positions come from `Node::rect().center()` on the row and
the header, which keeps the test independent of the explorer's exact metrics.

The gesture turned out to be fully testable, so nothing here is left to a by-hand check. The
tests are in three places, each where its outcome is visible:

- **What each target does** is a `ui_harness` showing the explorer alone, with a project, a data
  source in it and one outside, asserting the `ExplorerAction`s the gesture pushes: the move onto
  a project header, the move onto **No project**, and nothing at all for a drop on the source's
  own group or on another data source's row. The actions are the only place those last two are
  observable — a move that does nothing writes a file that looks the same, so the saved file
  cannot tell a silent drop from a pointless one.
- **The whole chain** is one app test: drag a data source onto a project header and wait for
  `data_sources.toml` to show it in that project, pointer to file.
- **Auto-scroll** is a table-driven test of `drag_scroll_delta`, the pure function that turns the
  pointer's position in the view into a scroll delta — zero away from the edges, strongest at
  them, and no faster for a pointer past the edge than on it, so a drag that leaves the panel
  doesn't bolt. The `Ui` plumbing around it stays thin enough to read.

## Risks

- **The drag competes with the scroll area**, which also reads drags. The row's drag sense is on
  top of it, so the row wins where it overlaps; a drag that starts on empty space still scrolls.
- **A click becomes a drag** if the pointer moves during it. egui's threshold decides, the same
  threshold as everywhere else in the app, and the row keeps its click.
- **The drop target covers the whole header row**, including its expander, so it could have taken
  the clicks that fold the group. It senses hover only, which leaves clicks to the widgets under
  it; a test folds a project through it to keep that true.
- **Auto-scroll with no scrollbar**: the band is only read while a payload is in flight, so a
  list that fits the panel never moves.

## Demo note

Built. In the explorer, right-click a project header → **New data source ›** → **SQLite**, give
it a file and save: the data source appears under that header, with no move afterwards. The same
item is on **No project**'s header, where it creates in none, and the toolbar's `+` and the
Welcome tab are unchanged.

Then pick a data source up by its name and drag it onto another project's header: the header is
tinted and outlined while it will take it, and the name follows the cursor. Drop it and it moves.
Drop it on **No project** and it leaves its project; drop it on the header it already sits under,
or on another data source, a table or the **Reports** section, and nothing happens. With more
data sources than the panel shows, drag one against the top or bottom edge and the list scrolls
while you hold it there. Expanded or collapsed makes no difference, and the row keeps its click,
double-click and context menu.

Covered by explorer tests of what each kind of target does (the two moves, and silence over its
own group and over another data source), an app test that drags a data source onto a header and
waits for `data_sources.toml` to show it in that project, an app test of the whole create-in-a-
project flow from the header's menu to the saved file, a test that the header's expander still
folds the group under the drop target, and a table test of the edge-band scrolling.
