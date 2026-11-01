# Look and feel

DbRover's interface takes its layout and density from current desktop database tools
(TablePlus, Beekeeper Studio, DataGrip's new UI), and its colours from the brand palette
([brand.md](../brand.md)): a calm base, a few accents used sparingly. The code is in `crates/dbrover-app/src/theme.rs`, `fonts.rs`, `icons.rs`, `widgets.rs` and `welcome.rs`.

## Themes

There is a light and a dark theme. DbRover follows the OS setting until the user picks one in
**View → Theme** (Match system, Light, Dark). egui stores that choice with the rest of its
state in `app.ron`, so there is no setting of DbRover's own to keep in sync.

Both themes are built by one function from a `Palette`, a struct of colours by role:

| Role | Used for |
| --- | --- |
| `primary` | Selection, focus rings, the active tab, the main button on a screen, table icons |
| `secondary` | SQL keywords, links, database and schema icons |
| `success`, `warning`, `error` | Status only: connection dots, run results, log levels |
| `base` | The window behind the panels: the gaps between them, the menu and status bars |
| `panel` | Panels, windows and menus |
| `field` | Text fields, the SQL editor and the results grid |
| `control`, `control_hover` | Buttons and other controls at rest and under the pointer |
| `border`, `text`, `text_weak`, `selection`, `on_primary` | What the names say |

Widgets ask for a role, never a fixed colour, through `Palette::for_visuals(ui.visuals())`. A
colour that only works in one theme can't sneak in that way.

**Contrast is tested.** `theme::tests::text_colours_meet_wcag_aa` checks every text colour
against every background it is drawn on, in both themes, for WCAG AA (4.5:1). It caught the
first selection colours: SQL strings and keywords on selected text fell to 3.5:1. When adding a
role or a new text-on-background combination, add the pair to `text_pairs`.

**On macOS, selection and focus take the system's accent colour** (`macos::look`), read from
`NSColor.controlAccentColor` and read again when the window comes back to the front or the theme
changes. The selection fill is the accent faded into the field only as far as needed for plain
and SQL text on it to keep WCAG AA (`theme::accent_selection`), and
`theme::tests::accent_selection_meets_wcag_aa` checks it for each of macOS's accent colours. The
brand colours stay for icons, tabs and buttons.

## Panels as islands

The dock's background is the darker `base`, and each dock node (its tab bar and body) is a
`panel`-coloured rectangle with rounded corners and a 6-point gap around it, so tool windows
read as separate "islands". egui_dock draws this from the style `theme::dock_style` builds;
the gap is the separator, painted in `base`.

- Tool-window tabs (Explorer, Welcome, Log console) have a coloured icon and a semibold title.
  Console tabs use the regular weight, like editor tabs.
- The active tab is a faint primary tint rather than a frame or underline.
- egui_dock's per-node "collapse" and "close all" buttons are hidden: they were the heaviest
  marks in the tab bar and the actions they offer are rarely what you want.
- Below each tab bar is a compact row of small, monochrome icon buttons (`widgets::toolbar`,
  `IconButton`). An icon button's tooltip and accessible name are its label, so screen readers
  and UI tests find "Run all" rather than a private-use character.
- The status bar is a single line of small, weak text with a dot for the engine's state.

## Buttons, menus and dialogs

Controls are 24 points tall and buttons padded 10 × 4 (`theme.rs` names each value), roomier
than egui's defaults but still dense where the data is. The design is in
[Buttons and menus](../buttons-and-menus.md).

- **Buttons have a role.** A secondary button (egui's `ui.button`) and a field are outlined
  in `border` at rest. A screen's one main action is `form::primary_button`, filled with
  `primary`; an action that destroys something is `form::danger_button`, filled with `error`.
  Both step towards the text colour when hovered and pressed (`Palette::hovered`,
  `Palette::pressed`), and text on every step is held to WCAG AA. Icon buttons are square.
- **Menus are DbRover's, not egui's.** egui's own menu style pads entries by 2 × 0 points;
  `theme::menu_style` gives them a control's height plus a little, button padding, and a
  hover highlight with no gaps between entries. Open menus with `widgets::context_menu` and
  `widgets::menu_button`, never `Response::context_menu` or `Ui::menu_button`, so a menu and
  its submenus take the style.
- **Dialogs open through `widgets::dialog`**, which gives a `Modal` the window margin instead of
  egui's popup frame, sized for menus.

## Typography

Three typefaces ship in `assets/fonts/`, each with its OFL licence:

- **Inter** (regular and semibold) for the interface. egui has no font weights, so semibold is
  a font family of its own (`fonts::semibold`), used for headings and tool-window titles.
- **JetBrains Mono** for SQL, result cells and the log console.
- **Phosphor** (from the `egui-phosphor` crate) for icons.

On macOS the real window uses the system's own faces instead: SF Pro and SF Mono, read at
runtime from `/System/Library/Fonts` and never shipped (`fonts::install_system`). Both are
variable fonts, so each family is the file at set coordinates: SF Pro at its text optical size
(17; the file defaults to its display cut) and weights 400 and 590, SF Mono at 400 (it defaults
to its lightest weight). If a file is missing or has no weight axis, the bundled faces stay and
the log says why. UI tests and the manual's pictures always use the bundled faces.

Body text is Inter (or SF Pro) at 13 points; spacing is tighter than egui's defaults so more tree and grid
rows fit on screen.

**Icons have their own font family** (`fonts::icon`, `widgets::glyph`), and are deliberately
not a fallback of the text families, which is how `egui-phosphor` installs them by default.
Inter has its own glyphs at over 300 of the private-use code points Phosphor uses, so behind
Inter many icons came out as accented letters (a database icon became "⇔", a folder "ṡ").
Phosphor can't go in front of Inter either, because it maps a–z to the blank glyphs its
ligatures are built from. So: every icon goes through the icon family, and a raw icon character
in an ordinary label shows as a missing-glyph box, which is easy to spot.

## Icons and colour

`icons.rs` maps each driver and each catalog node kind to a glyph and a palette role:

| Kind | Icon | Colour |
| --- | --- | --- |
| Database, schema | database, tree | secondary |
| Folder | folder | warning (amber) |
| Table, view, materialized view | table, eye, stack | primary |
| Function, sequence | function, numbered list | success |
| Column | columns | weak text |
| Index, constraint | magnifying list, key | warning |
| Trigger | lightning | error |

**Data sources show their database's logo** rather than a generic icon: on the explorer row,
in the New data source menu, on the Welcome tab's console menu and in the data source dialog's
title. The logos are [Simple Icons](https://simpleicons.org) SVGs (CC0), kept in
`assets/icons/databases/` with their source, version and the one change made to them, and
rendered by egui_extras' SVG loader (`resvg`). They are single-colour shapes, filled white and
tinted per theme by `Palette::driver`: the brand's colour in the light theme, a lighter shade
of the same hue in the dark one. `theme::tests::database_logos_stand_out_from_what_they_are_drawn_on`
holds them to WCAG's 3:1 for graphics. Console tabs keep the SQL-file icon in the data source's
colour, because egui_dock's tab titles are text and can't hold an image.

We chose SVG files over an icon font (a Nerd Font, say): Simple Icons covers the databases
DbRover plans to support (MySQL, MariaDB, ClickHouse, DuckDB) under one licence, while Nerd
Fonts mix icon sets under several licences, cover databases unevenly, and would mean embedding
a large font for a handful of glyphs. Oracle is the exception: it had its logo removed from
Simple Icons, so an Oracle driver will need another source.

Loaded folders show how many children they have as a small, weak badge after the label
(`tables 4`), counting system objects only while they are shown.

A data source's status dot sits after its name: amber while connecting, green when connected,
red when the last attempt failed, and no dot when disconnected, so the eye goes to the sources
that are live or in trouble.

## Data source colours

A data source can be given one of eight colours in its dialog (red, orange, yellow, green,
teal, blue, violet, pink), stored as `color = "red"` in `data_sources.toml`. The colour tints
the data source's explorer row, its console tabs (fill and outline) and a thin band along the
top of each of its consoles, so a console on production is hard to mistake for one on a local
copy.

It is a fixed set of names (`dbrover_core::SourceColor`) rather than free RGB: each name has a
light-theme and a dark-theme shade (`Palette::source`), so a colour picked in one theme still
works in the other. The field is optional and serde-defaulted, so files written before it
existed still load, and it is only written when set.

## Welcome tab

The Welcome tab shows the logo over two soft radial glows (secondary on the left, primary on
the right, drawn as triangle fans because egui has no gradient fills), a title, three square
cards — **New data source**, **New query console** and **Open the manual** — and a short card
with a solid primary button to the manual. The card a new user most likely wants next has an
accent border: New data source while there are none, New query console after that. New query
console is disabled until a data source exists.

## Results grid and syntax colours

The grid's header row is tinted with the primary, rows are striped, and cells use JetBrains
Mono, with numbers right-aligned and `NULL` in weak italics. Syntax colours come from the
palette: keywords secondary, strings success, numbers warning, quoted identifiers primary,
comments weak and italic.

## Screenshots

Pictures of the UI, including every image in the user manual (`book/`), are rendered, not
taken by hand: `screenshots::regenerate_screenshots` (ignored by default) drives the app
headlessly with egui_kittest's wgpu renderer against a SQLite fixture, and a PostgreSQL data
source when `DBROVER_TEST_POSTGRES_URL` is set. See
[testing.md](../testing.md#screenshots).
