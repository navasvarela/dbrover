# A native look on macOS

Status: **built** (2026-10-27); what changed while building it is under [As built](#as-built). This is the design of D5 in the
[roadmap](roadmap.md#d5--a-native-look-on-macos).

DbRover draws everything with egui, so on macOS it looks like an egui app in a Mac window: a
menu bar inside the window, a grey title bar above it with the word "DbRover", Inter instead of
the system font, and teal selection whatever accent colour the user picked. D5 makes it read as
a Mac app while keeping egui for everything inside the window, and leaves Windows and Linux as
they are.

## What stays the same

- **egui draws the window's contents** ([ADR 0001](adr/0001-ui-toolkit-egui.md)). Nothing here
  replaces a panel with a native control; the menu bar and the title bar are the window's
  chrome, not its contents.
- **One set of commands.** A menu item does exactly what the same item does in the egui menu
  and what its shortcut does, through one function, so the two menus can't drift apart.
- **The keymap still decides shortcuts** ([keymap](features/keymap.md)). A menu item for a
  keymap action shows, and answers to, the keys `keymap.toml` gives it, in either menu.
- **Windows and Linux keep the egui menu bar** and today's fonts and colours. macOS-only code is
  behind `cfg(target_os = "macos")`, and CI still builds and tests all three systems.
- **Tests and the manual's pictures don't depend on the machine.** The native menu, the
  integrated title bar, the system font and the accent colour are switched on by `main` for the
  real window only. UI tests and `screenshots::regenerate_screenshots` build the app without
  them, so they render the same bundled fonts and palette on every system, and the existing UI
  tests that click through the egui menu keep working on macOS.

## D5a — One list of commands

A `commands` module lists what the menus can do: `AppCommand` (Open…, Save, Export data
sources…, Theme › Light, Format, About DbRover, and so on), each with

- a stable id (`file.open`, `view.theme.dark`), for the native menu's item ids and for tests;
- its label, as the egui menu shows it today;
- its shortcut: the keymap action it runs (`keymap::Action::Save`), so a rebinding applies to
  the menus too, or none;
- whether it is enabled and checked, from a small `CommandState` (is a console focused, are
  there data sources, which theme is picked) that the app fills in once per frame;
- its tooltip and the reason it is disabled.

The menu bar itself is data too: `commands::egui_menu_bar` returns the menus, their entries,
separators and submenus (with Open recent's files), and `app.rs` walks it to draw egui's
`MenuBar`. Every click, and the keymap's file shortcuts, go through one
`DbRoverApp::run_command`. This layer changes no behaviour: the egui menu has the same items,
shortcuts, tooltips, enabled states and icons as before, which kittest tests pin down.

## D5b — The macOS menu bar

On macOS DbRover installs a real menu bar with [`muda`](https://crates.io/crates/muda) 0.21.2,
the menu crate Tauri uses, and hides the egui one. `commands::macos_menu_bar` arranges the same
commands the way Mac apps do:

| Menu | Items |
| --- | --- |
| **DbRover** | About DbRover, Check for updates…, Settings… ⌘,, Services, Hide DbRover ⌘H, Hide Others ⌥⌘H, Show All, Quit DbRover ⌘Q |
| **File** | Open… ⌘O, Open recent ›, Save ⌘S, Save as… ⇧⌘S, Export data sources…, Import data sources… |
| **Edit** | Undo ⌘Z, Redo ⇧⌘Z, Cut ⌘X, Copy ⌘C, Paste ⌘V, Select All ⌘A |
| **View** | Theme ›, Keymap…, Reset layout, Enter Full Screen ⌃⌘F |
| **Code** | Format ⌥⌘L, Format all ⇧⌥⌘L |
| **Window** | Minimize ⌘M, Zoom, Bring All to Front |
| **Help** | DbRover manual (and macOS's menu search) |

A unit test checks that every command in the egui menu is somewhere in the macOS one.

- **Events.** muda calls a handler on the main thread when an item is chosen; it forwards the
  item's id over a channel and asks egui for a repaint, and the next frame runs the command.
- **Edit goes to the focused egui widget.** muda's predefined Copy and Paste send `copy:` and
  `paste:` to the first responder, which is winit's view, and it doesn't implement them, so they
  would always be greyed out. DbRover's own Edit items instead add to the frame's input what the
  keys would have: `Event::Copy`, `Event::Cut`, `Event::Paste` with the pasteboard's text, and
  ⌘Z, ⇧⌘Z or ⌘A key presses (in `raw_input_hook`, before any widget runs). The editor, text
  fields and the results grid handle them as they handle the keys today.
- **Quit asks first.** Quit is DbRover's own item and closes the window the usual way, so open
  transactions and unsaved files are still confirmed. winit's default menu sends `terminate:`,
  which skips that.
- **State stays in sync.** Each frame the app updates enabled and checked marks that changed,
  the shortcuts after `keymap.toml` is reloaded, and Open recent when the list changes.
- **Settings… moves to the app menu with ⌘,** (Ctrl+, in the egui menu on Windows and Linux),
  as a new keymap action `app.settings`.
- **The dependency.** muda is a macOS-only dependency without default features, so Linux
  doesn't pull in GTK. Its macOS dependencies (`objc2` 0.6, `objc2-app-kit` 0.3,
  `crossbeam-channel`, `dpi`) are already in the lockfile through winit and rfd; it adds
  `keyboard-types`. Writing the menu against `objc2-app-kit` by hand would mean our own
  `NSMenuItem` target class and key-equivalent mapping, which is the part muda gets right.

## D5c — An integrated title bar

On macOS the window's content runs under a transparent title bar
(`with_fullsize_content_view`, `with_titlebar_transparent`, `with_title_shown(false)`), so the
traffic-light buttons sit on DbRover's own top strip, as in Finder, Xcode or TablePlus:

- The strip is as tall as a title bar (28 points), leaves 78 points on the left for the
  buttons, and shows the window title centred in weak text.
- Dragging the strip moves the window (`ViewportCommand::StartDrag`), and double-clicking it
  zooms (`ViewportCommand::Maximized`), as the title bar did.
- In full screen the buttons hide, so the inset goes too.
- Without a native menu (tests) the egui menu sits in the strip after the inset, so the strip
  can be tested with kittest on any system.

## D5d — The system font and accent colour

- **SF Pro and SF Mono**, read at runtime from `/System/Library/Fonts/SFNS.ttf` and
  `SFNSMono.ttf`, never bundled: Apple's licence allows using them as the system font, not
  shipping them. SF Pro is a variable font; the semibold family is the same file at weight 590
  (`FontTweak::coords`). If either file is missing or doesn't parse, DbRover keeps Inter and
  JetBrains Mono, and says so in the log.
- **The accent colour** (`NSColor.controlAccentColor`, through `objc2-app-kit`) replaces teal
  for selection, focus rings and the text cursor. The selection fill is the accent faded into
  the panel, as the palette's own is, and a test holds text on it to WCAG AA for each of
  macOS's accent colours, in both themes. The brand colours stay for icons, tabs and buttons.
  DbRover reads the accent again when the window regains focus or the theme changes, so a change
  in System Settings shows without a restart.

## D5e — The manual

The book describes the macOS menu bar, where Settings… and Check for updates… are on a Mac,
the Edit menu, and that DbRover uses the system font and accent colour there. Pictures stay
rendered with the bundled fonts.

## Decisions

- **muda, not a hand-written `NSMenu`.** See D5b. egui has no native menu of its own, and
  eframe's winit only offers its default app menu.
- **Edit items are DbRover's, not muda's predefined ones,** because winit's view doesn't answer
  `copy:`/`paste:` (see D5b).
- **The native chrome is opt-in from `main`,** not decided inside `DbRoverApp::new`, so tests on
  a Mac see the same app as on Linux, and the manual's pictures don't change with the machine
  that renders them.
- **Accent for selection and focus only.** Tinting every primary-coloured mark with the accent
  would make a pink-accented DbRover lose its brand; selection and focus are what macOS itself
  tints.

## Risks

- **Key equivalents swallow keys.** A menu item's shortcut is handled by the menu before the
  window sees the key, so egui no longer gets ⌘S, ⌘C or ⌘Z directly. That is why every menu item
  runs exactly what egui would have, and why the Edit items feed egui the same events. A keymap
  binding that can't be written as a menu key equivalent is left off the item and keeps working
  through egui.
- **Paste reads `NSPasteboard` itself,** since egui-winit's clipboard isn't reachable from the
  app. Only plain text is pasted, as before.
- **System font files can move.** Apple has renamed them before (`SFNSDisplay.ttf`,
  `SFNSText.ttf`); the fallback keeps the app usable, and the log names the missing file.
- **Double-click on the strip always zooms,** ignoring the "double-click a window's title bar
  to minimize" preference.
- **No macOS UI tests.** CI's macOS runner has no window server session to drive a menu bar;
  the native parts are checked by running the app, and the pure parts (the command list, the
  menu layout, accent contrast) by unit tests on every system.

## As built

- **Menus are data in `commands`**: `egui_menu_bar` and `macos_menu_bar` return `Menu`s of
  `Entry`s (a command, an Edit command, an AppKit item, a separator, a submenu, or a greyed-out
  note). `menu_bar.rs` draws the first with egui; `macos/menu.rs` builds the second with muda.
  `native_menu.rs` and `system_look.rs` are the cross-platform faces of the macOS code: on other
  systems their types have no values, so `app.rs` needs no `cfg`s.
- **The native menu is rebuilt** when the recent files or the keymap change, which is rare;
  enabled and checked marks are compared every frame and only changes reach AppKit.
- **Edit input goes in through `raw_input_hook`**, before the frame, with the rest of the
  picked commands. Converting egui shortcuts to menu key equivalents (`macos::menu`) is tested
  on macOS only, where muda is built.
- **Settings… is the keymap action `app.settings`**, so the egui menu shows its key too: ⌘, on a
  Mac, "Ctrl+Comma" (egui's spelling) elsewhere.
- **egui 0.36 spells the transparent title bar `with_titlebar_shown(false)`**; egui-winit turns
  it into winit's `with_titlebar_transparent(true)`.
- **The system faces need coordinates for every family**, not only semibold: `SFNS.ttf`
  defaults to its display optical size (28) and `SFNSMono.ttf` to its lightest weight
  (294.67). The text cut (optical size 17) and weights 400 and 590 are set explicitly, and a
  file without a weight axis is refused, so the bundled faces stay.
- **The accent selection isn't a fixed tint.** A fixed 20 % (light) or 30 % (dark) left SQL
  strings, keywords and comments below WCAG AA for most of macOS's accents, so the fill takes at
  most that much and fades further until every text colour on it passes; a test also keeps it
  visibly different from the field.
- **Tried by eye on 2026-11-03,** with the released app on a Mac. The session that built this
  had no screen-recording or accessibility access, so until then the native menu bar, the window
  buttons over the strip, and dragging and zooming had not been seen running: only that the app
  started and installed the menu without errors, and the fonts and accent colour in an image
  rendered with them.
