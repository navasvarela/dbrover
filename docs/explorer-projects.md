# Data sources into projects

Status: **planned** (2026-11-20). This is the design of D9 in the
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
| What it looks like while dragging | The source's name follows the cursor, egui's `dnd_drag_source` ghost; its row keeps its place, its expander and status dot | Shows what is in flight and where it came from, without the list reflowing under the cursor. |
| Valid target hovered | The header row fills with the accent at low opacity and takes a 1-point accent outline, the full width of the panel | The same row-wide fill the selected data source uses, in the accent rather than the selection colour, so "drop here" and "selected" don't look alike. |
| The source's own group | No highlight, and dropping does nothing | A move to where it already is. **Move to project ›** already shows the current project checked and does nothing when it is picked; the drag says the same thing by staying quiet. |
| Any other target | Not a drop zone: no highlight, and a release over it moves nothing | A data source row, a table, **Reports**, **Query folders** — nothing claims the payload, so the drag ends where it started. There is no error to report: the user pointed at something that isn't a project. |
| A list taller than the panel | Dragging into the top or bottom 24 points of the explorer scrolls it, faster the closer to the edge | Without it a project folded out of sight is unreachable by drag, and the gesture would be a dead end exactly when the list is long enough for the gesture to be worth having. |
| Dropping outside the explorer | Nothing | egui drops the payload when the button is released over anything that doesn't read it. |

### How it is built

egui 0.36 has the whole mechanism; D9 adds the gesture, not a drag framework.

- The name label is wrapped in `Ui::dnd_drag_source`, keyed by the data source's id, with a
  payload of the dragged source's id and its current project. It paints the ghost and keeps the
  row's click, double-click and context menu, because `dnd_drag_source` ORs its drag response
  with the body's own.
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

The tests are on a `ui_harness` showing the explorer alone, with two projects and a data source
in each, asserting the `ExplorerAction`s the gesture pushes: the move, the silence when the
target is the source's own group, the silence when the release lands on another data source, and
the scroll offset changing when the pointer is held in the edge band.

## Risks

- **The drag competes with the scroll area**, which also reads drags. The row's drag sense is on
  top of it, so the row wins where it overlaps; a drag that starts on empty space still scrolls.
- **A click becomes a drag** if the pointer moves during it. egui's threshold decides, the same
  threshold as everywhere else in the app, and the row keeps its click.
- **The ghost leaves its slot empty** while the name follows the cursor. Deliberate, and the row
  keeps its expander, logo and status dot so it doesn't look deleted.
- **Auto-scroll with no scrollbar**: the band is only read while a payload is in flight, so a
  list that fits the panel never moves.

## Demo note

Written once D9 is built.
