# Settings

**View → Settings…** (**DbRover → Settings…**, ⌘, on a Mac) opens the settings in a tab beside
your consoles. Asking again brings the same tab to the front; close it like any other tab.

![Settings, on the Editor page](images/settings.png)

The sidebar lists the categories:

- **Appearance**: the theme, matching the system or always light or dark, as **View → Theme**
  chooses it.
- **Editor**: how [Format SQL](consoles.md) writes keywords and indents, and which
  [problems](consoles.md) the console points out while you type.
- **Query history**: how long [the history](history.md) keeps statements.
- **MCP**: what [AI assistants](mcp.md) may do in a project that doesn't say otherwise.
- **Updates**: whether DbRover checks for a new release when it starts.

Below them, **Keymap…** and **Custom actions…** open the [keymap](shortcuts.md) and
[your actions](customising.md).

Type in **Search settings** to find a setting in any category by a word it shows, or one it is
known by: *dark*, *indent*, *timeout*. The sidebar counts what each category has that matches;
click a category to leave the search.

Every change is saved at once in `settings.toml` (see
[Where DbRover keeps its files](files.md)), except the MCP defaults, which wait for **Save MCP
defaults**, and the theme, kept with the window's layout. After editing `settings.toml` by hand,
click **Reload** at the bottom of the page.
