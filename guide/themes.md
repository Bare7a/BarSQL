# 🎨 Themes

BarSQL ships 20 themes, uses one for dark mode and one for light mode, and loads your own from the data folder.

## 🖌️ Picking a Theme

- **View ▸ Theme** - **Dark**, **Light** or **Match system** decides the mode, and the **Dark theme** and **Light theme** lists decide which theme each mode uses
- **Select theme…** (also in the command palette) lists every theme and shows each one as you move to it with the arrow keys or by typing. **Enter** keeps it, **Esc** goes back to what you had. Typing `dark` or `light` narrows the list to that mode
- Picking a light theme while in Dark switches to Light, and the other way round. Under **Match system** the pick waits until the system switches, and a notice says so

## 📚 Built-in Themes

| Dark | Light |
| ---- | ----- |
| BarSQL Dark | BarSQL Light |
| Catppuccin Frappé, Macchiato and Mocha | Catppuccin Latte |
| One Dark | One Light |
| Dracula | |
| Tokyo Night | Tokyo Night Light |
| Nord | |
| Gruvbox Dark | Gruvbox Light |
| Solarized Dark | Solarized Light |
| VS Code Dark Modern and Dark+ | VS Code Light Modern and Light+ |

Each comes from its original project's palette, under the MIT license. Where a palette's own text, lines, line numbers, comments, selections or status colors are too faint for a data grid, BarSQL uses a slightly stronger shade of the same color. Their licenses sit next to the theme files in `crates/barsql-ui/themes/`.

## ✏️ Making Your Own

The quickest start is **View ▸ Theme ▸ Customize current theme…**. It saves a copy of the theme you're using to the `themes/` folder, switches to it and opens the file. Edit it, save, click back into BarSQL, and you'll see the change. On a theme that's already yours, it opens that theme's file instead of copying it again.

You can also put your own `.json` files in **`BarSQL-data/themes/`**. **Open themes folder** creates it and opens it. A file holds one or more themes:

```json
{
  "name": "My Themes",
  "author": "You",
  "themes": [
    {
      "name": "Midnight",
      "mode": "dark",
      "colors": {
        "background": "#101418",
        "foreground": "#d8dee9",
        "border": "#2a3038",
        "muted.foreground": "#8a93a0",
        "primary.background": "#5fb3b3",
        "primary.foreground": "#101418",
        "sidebar.background": "#0c1014"
      },
      "highlight": {
        "editor.background": "#101418",
        "syntax": {
          "keyword": { "color": "#c594c5" },
          "string": { "color": "#99c794" },
          "comment": { "color": "#65737e", "font_style": "italic" }
        }
      }
    }
  ]
}
```

- **Every theme** needs a `name` and a `mode`, `"dark"` or `"light"`. The name is what BarSQL saves when you pick the theme, so renaming it later switches back to BarSQL's own until you pick it again
- **`colors`** sets the interface. Anything you leave out is worked out from the main colors: `background`, `foreground`, `border`, `muted.background`, `primary.background`, `secondary.background`, `overlay` and the six hues `base.red`, `base.green`, `base.blue`, `base.yellow`, `base.magenta` and `base.cyan`
- **Button text** is `primary.foreground`. Left out, it's your `foreground`, so set it whenever that doesn't read on your `primary.background`
- **Gradients**: a color that fills an area, such as `primary.background`, also takes `linear-gradient(135deg, #5fb3b3, #3a8f8f)`, with an angle and two colors
- **`highlight`** sets the editor (`editor.background`, `editor.foreground`, `editor.active_line.background`, …) and, under `syntax`, the SQL colors: `keyword`, `operator`, `boolean`, `constant`, `string`, `string.special`, `number`, `comment`, `comment_doc`, `punctuation`, `punctuation.delimiter`, `punctuation.bracket`, `variable`, `property`, `function`, `type`, `attribute` and `tag`. Each takes a `color`, plus an optional `font_style` (`"italic"`) or `font_weight`. In SQL, table names use `type`, aliases use `variable` and column names the editor's text color
- **Syntax colors you leave out** come from BarSQL Dark or BarSQL Light, except under a color you set: `punctuation` also colors `punctuation.delimiter` and `punctuation.bracket`, `string` colors `string.special`, and `comment` colors `comment_doc`, until you set those too. Leave out `editor.background` and the editor takes the color of the theme's input fields, so set it to match your `background`
- **Fonts, sizes and corners** stay BarSQL's, so any set in a theme file are ignored
- **More keys**: a file made by **Customize current theme…** lists every key BarSQL's own themes set, the quickest way to see what you can change
- Only plain JSON loads: no comments and no trailing commas. Files over 1 MB, subfolders and editor backups such as `mine.json~` are skipped

BarSQL reads the folder again whenever its window comes back to the front and a file has changed. If a file you're editing stops being valid JSON, can't be read or loses its `themes` list, the last version that loaded stays in use until you fix it. Other mistakes skip only the theme or value they're in.

## 🔔 Notices

When a file has a problem, a notice names the file and its first problem, with an **Open file** button.

| Notice | What to do |
| ------ | ---------- |
| *line 3, column 14: …* | The file isn't valid JSON there. None of its themes load until it's fixed |
| *couldn't be read* | Check the file's permissions and that it's saved as UTF-8 |
| *is over 1 MB* | A theme is a few KB; the file was skipped |
| *has no themes* | The file needs a `"themes": [ … ]` list |
| *a theme has no name*, *needs "mode"* | That theme is skipped until it has both |
| *is a built-in theme's name*, *is already used by another theme* | Rename it. Names are compared ignoring case and spaces at either end |
| *… isn't valid*, *… isn't a theme key* | That one value is ignored and the rest of the theme loads. Usually a typo |
