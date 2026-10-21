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
