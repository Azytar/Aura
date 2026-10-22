# Configuration

Aura loads `~/.config/aura/config.toml` (or `$XDG_CONFIG_HOME/aura/config.toml`).
Use `aura --config-file PATH` to select another file. `aura --print-config` prints
an editable example, and `aura --config-file PATH --check-config` validates it
without opening a display or starting a child. `--doctor` also accepts that path.

Every section is optional. Unknown names, wrong types and invalid bounds produce
an error with the affected key. See `example/config.toml` for accepted settings,
defaults and bounds. Values are literal; paths beginning with `~/` expand the
home directory where shell paths and imports accept them.

## Imports and reload

```toml
[general]
import = ["themes/dark.toml", "machine.toml"]
live_config_reload = true
```

Imports resolve relative to the containing file. Later files override earlier
files; the importing file overrides them all. Nested tables merge recursively.
Arrays, including binding arrays, replace an earlier array completely. Cycles,
unreadable imports and nesting beyond sixteen files are errors. Every imported
file is validated, so overriding a value cannot hide an unknown key.

Loaded files and imports are watched. A valid edit updates fonts, cell spacing,
padding, palette, cursor settings, pointer policy, scrollback capacity, bindings
and window identity. Invalid edits keep the active settings. Ctrl+Shift+R retries
a reload, including when automatic reload is disabled. A newly added import that
is initially unavailable may require this explicit retry after creating it.

Shell, child environment and initial columns/lines apply at startup; reload does
not replace the running process or force the outer window to its startup size.
A font size change resets zoom to that size; other edits preserve current zoom.
Application cursor-shape choices remain active unless cursor defaults change.

## Bindings

```toml
[keyboard]
use_default_bindings = true

[[keyboard.bindings]]
key = "F6"
mods = "Control|Shift"
mode = "Main"
chars = "example"
```

`key` accepts an XKB keysym name such as `Return`, `Page_Up`, `KP_Add` or a single
Unicode character. Modifiers match exactly; join `Shift`, `Alt`, `Control` (`Ctrl`)
and `Super` with `|`. Omit `mods` for no modifier. `mode` is `Any`, `Main` or
`Alternate`. Custom bindings take precedence; the last match wins.

Set exactly one of `chars` or `action`. Supported actions are `Copy`, `Paste`,
`PasteSelection`, `IncreaseFontSize`, `DecreaseFontSize`, `ResetFontSize`,
`ScrollPageUp`, `ScrollPageDown`, `ScrollTop`, `ScrollBottom`, `ReloadConfig`
and `None`. `None` disables a default shortcut. `chars` sends the supplied bytes
directly; TOML basic strings can represent escapes such as `"\u001b[H"`.

Without a matching local binding, enhanced keyboard encoding is negotiated by
the application. Plain shells retain legacy input. Shift also bypasses application
mouse tracking for local selection; ordinary hover/motion reports remain opt-in.

Holding the left button in the first or last text row scrolls the selection
through retained history, even when the pointer cannot leave the screen.
Releasing that button stops the scroll; wheel events can be used during the drag.
A selection started with Shift retains local ownership until button release,
including edge autoscroll, even if Shift is released first.

The mouse pointer follows the session's theme and size. For each setting,
explicit `XCURSOR_THEME` / `XCURSOR_SIZE` take precedence over Xresources
(`Xcursor.theme` / `Xcursor.size`), then XSettings (`Gtk/CursorThemeName` /
`Gtk/CursorThemeSize`), then GTK defaults (`gtk-cursor-theme-name` /
`gtk-cursor-theme-size` in the `[Settings]` section). GTK defaults are read from
`gtk-3.0/settings.ini` and `gtk-4.0/settings.ini` under `$XDG_CONFIG_HOME`
(otherwise `~/.config`), `$XDG_CONFIG_DIRS` (otherwise `/etc/xdg`), and `/etc`,
in that order; GTK3 precedes GTK4 within each location.

Xresources/XSettings updates apply through X11 events. GTK files use the existing
one-second configuration tick, including atomic file replacement; changing the
desktop pointer does not require restarting Aura or enabling TOML live reload.
No environment queries or theme loads run per keypress/frame. Theme search
follows `XCURSOR_PATH` or standard icon directories. Missing/malformed settings
and images retain a visible fallback. These settings affect the mouse pointer;
`colors.cursor` and `[cursor]` configure the terminal's text cursor.

## Shell and environment

```toml
[shell]
program = "/bin/zsh"
args = ["-l"]
working_directory = "~/projects"

[env]
EDITOR = "nvim"
```

Arguments are passed directly, without an intermediate command-line parser.
The directory and environment apply only to the child. With no explicit program
or arguments, Aura keeps its configured interactive login-shell selection.

## Repaint scheduling

`window.max_fps` limits render calls to 1..1000 per second (default 120), with
a short initial coalescing delay. Input and PTY processing continue between
repaints; an idle terminal does not poll for frames. This is a ceiling, not a
promise of presented frames or display latency. The setting supports live reload.
Typing alone does not repaint an unchanged, visible cursor. Removing a selection,
returning to the live view or restoring a hidden blink phase still repaints even
when the application has disabled echo; application output schedules its own frame.

## Fonts and text

Font names search `$XDG_DATA_HOME/fonts` (default `~/.local/share/fonts`),
`~/.fonts`, `/usr/local/share/fonts` and `/usr/share/fonts`. Explicit file paths
take precedence. Fontconfig aliases and rules are not evaluated. Style faces and
fallback families use the same discovery; fallback preserves the primary grid.

`colors.cursor` overrides the block, bar and underline paint. With no override,
the cursor uses the effective cell foreground. Unfocused windows show a steady
hollow cursor. Regaining logical focus restores the configured shape, including
window-manager focus moves during keyboard grabs. Temporary grabs and focus
moves to a child do not show an unfocused cursor or emit focus reports.
`cursor.blink_interval_ms` sets the interval; typing restarts it.
After `cursor.blink_timeout_ms` of inactivity the cursor stays visible; zero
disables that timeout. Applications can still select cursor shape and blinking.
