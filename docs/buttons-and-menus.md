# Buttons and menus

Status: **built** (2026-11-01). This is the design of D7 in the
[roadmap](roadmap.md#d7--roomier-buttons-and-menus).

D2 gave DbRover its colours and D5 its macOS chrome, but the controls inside the window kept
egui's proportions: "it all seems too crowded and unloved". Looking at every screen in both
themes, the crowding comes from a handful of places, and each of them is one value or one
helper rather than something every screen got wrong.

## What feels crowded

- **Menus and context menus.** egui draws every menu with its own `menu_style`, which sets the
  button padding to 2 × 0 points whatever the app asked for. Entries are text with a highlight
  that touches it, and the app's 5-point item spacing leaves gaps between highlights as the
  pointer moves down a menu. A menu of two short entries is as narrow as its text.
- **Buttons don't read as buttons.** A button at rest is `control` on `panel` with no
  outline: in the light theme that is #ECEEF2 on #FAFBFC, so **Cancel**, **Export…** or
  **Add row** look like tinted text. Fields have the same problem: the data source dialog's
  fields are white on near-white, and only the Settings tab's cards gave them an outline.
- **One kind of button.** Only the export dialog and the MCP page use `form::primary_button`.
  Seven dialogs fill their main button by hand, with no hover state, and the rest leave it plain.
  **Delete** in a confirmation looks like **Cancel** beside it.
- **Dialogs have a menu's margin.** egui's `Modal` frames itself with `Frame::popup`, which
  takes the menu margin (6 points), so a dialog's heading, text and buttons sit against its
  edge. Windows (`Window`) already get the 14-point window margin.
- **Icon buttons** are as wide as their glyph, so their hover highlight is a narrow pill.

## Direction

Roomier, still dense: DbRover is a tool people keep open all day beside a lot of data, so the
tree and the grid keep their density, and controls get the room that makes them read as
controls. The pictures in the plan's pull request show each screen before and after, in both
themes.

| Token (`theme.rs`) | Before | After |
| --- | --- | --- |
| Control height (`interact_size.y`) | 22 | 24 |
| Button padding | 8 × 3 | 10 × 4 |
| Item spacing | 8 × 5 | 8 × 6 |
| Window and dialog margin | 14 (dialogs: 6) | 16 |
| Menu entry | 2 × 0 padding, 22 tall, 5 apart | 8 × 4 padding, 26 tall, 1 apart, 160 wide at least |
| Menu bar title | 2 × 0 padding | 8 × 3 padding, still 22 tall |
| Toolbar spacing | 2 | 3 |

- **Buttons by role.** A *secondary* button (egui's own `ui.button`) gets a one-point
  `border` outline at rest; so do fields, checkboxes and drop-downs, which share egui's
  "inactive" look. The *primary* button, one per screen, is filled with `primary`, and a
  *danger* button, for actions that destroy something (delete, drop, roll back, run a statement
  DbRover warned about), with `error`. Both step towards the text colour under the pointer
  and when pressed (`Palette::hovered`, `Palette::pressed`), so they answer the pointer like
  any other button. *Icon* buttons stay frameless at rest and become square tiles as tall as a
  control.
- **Menus in DbRover's style.** `theme::menu_style` replaces egui's: entries as tall as a
  control plus a little, padded like buttons, frameless until hovered, a hairline apart, with
  rounded highlights inside the menu's rounded frame. Every context menu and menu button goes
  through `widgets::context_menu` and `widgets::menu_button`, and the menu bar through
  `theme::menu_bar_style`, so submenus inherit the style and no screen can draw a menu the old
  way by accident.
- **Dialogs** go through `widgets::dialog`, which frames a `Modal` with the window margin.
  Their main action is `form::primary_button` and a destructive one `form::danger_button`.
- **The menu bar keeps its height.** egui opens a window it has no position for 16 points from
  the top of the screen, over the menu bar on Linux and Windows. At 24 points the titles' middle
  falls under a new window's resize edge, so the bar stays 22 points tall and only gets wider
  padding.

## What stays the same

- **Colours.** No palette value changes. The new pairs (text on hovered and pressed primary
  and error fills) are held to WCAG AA by `theme::tests::text_colours_meet_wcag_aa` with the
  rest.
- **Density where data is.** The results grid's rows, the SQL editor and the log console keep
  their own sizes. The explorer's rows follow the control height, two points taller.
- **The Settings tab's layout**, which is being reworked separately; it takes the new tokens
  like every other screen.
- **The macOS menu bar**, which the system draws (`native_menu.rs`).

## In layers

One pull request each, stacked:

1. **D7a — Plan:** this page and the roadmap entry, with before and after pictures.
2. **D7b — Spacing and sizing tokens:** the table above, as named constants in `theme.rs`.
3. **D7c — Buttons:** the outline at rest, `Palette::hovered` and `pressed`,
   `form::danger_button`, square icon buttons, and each dialog's main and destructive actions
   using them.
4. **D7d — Menus:** `theme::menu_style`, `menu_bar_style`, and the `widgets` menu helpers at
   every call site.
5. **D7e — Dialogs:** `widgets::dialog` at every modal; the manual's and the website's
   pictures rendered again.

## Risks

- **UI tests that click by position or wait a fixed number of frames** may need a step more
  with roomier layouts; the known kittest flake ("Harness::run exceeded max_steps") looks the
  same.
- **Every manual and website picture changes.** They are rendered again once, at the end,
  rather than in each layer.
- **The explorer shows a row or two fewer** in the same height; the grid, where rows matter
  most, doesn't change.
