# ὄμμα · omma

A configurable eye-strain break reminder for macOS. It lives in the menu bar,
schedules short and long breaks, dims the screen for each one with a countdown,
and gets out of your way when you're busy.

Written in Rust with [`objc2`](https://github.com/madsmtm/objc2); `unsafe` is
confined to the few framework FFI calls that have no safe wrapper.

## Features

- **Short and long breaks** on independent intervals (defaults: a short break
  every 15 min, a long break every 75 min), interleaved automatically.
- **Fullscreen break screen** on every display, with a live countdown and
  Skip / Postpone buttons (hidden in strict mode), each with a configurable
  keyboard shortcut (default: Esc to skip, Space to postpone).
- **Menu-bar item**: enable/disable, take a break now (short/long), open config,
  quit; shows the time until the next break.
- **Pre-break warning** — a small Liquid Glass dialog before each break, with
  Skip/Dismiss and optional Postpone actions.
- **Audible alerts** using system sounds.
- **Smart pause**: pauses the schedule when you're idle and resumes when you're
  back; a long idle stretch counts as a long break.
- **Do not disturb**: suppress breaks while you're busy (see below).
- **CLI control** of the running instance.

## Build & run

```sh
# Bundle and ad-hoc sign:
./bundle.sh
open ./Omma.app

# Optional: CLI on your PATH
ln -sf "$PWD/Omma.app/Contents/MacOS/omma" /usr/local/bin/omma
```

> The bare `cargo run` binary also works for development; bundling is the
> easiest way to launch omma like a normal menu-bar app.

## CLI

```text
omma                       start the menu-bar agent
omma --take-break  (-t)    take the next break now
omma --short-break (-s)    take a short break now
omma --long-break  (-l)    take a long break now
omma --enable      (-e)    enable the running instance
omma --disable     (-d)    disable the running instance
omma --quit        (-q)    quit the running instance
omma --status              print status of the running instance
omma --reload              re-read the config of the running instance now
omma --print-default-config   print the default config as TOML
```

The agent also watches the config file and reloads automatically within ~1s of
a save, so `--reload` is only needed for an instant, explicit refresh.

## Configuration

Config lives at `~/.config/omma/config.toml` and is created with defaults on
first run. See the full default with `omma --print-default-config`. Highlights:

```toml
# All times are in seconds.
short_break_interval = 900    # 15 min
long_break_interval  = 4500   # 75 min
short_break_duration = 15
long_break_duration  = 60
pre_break_warning    = 10     # warning shown before a break
postpone_duration    = 300    # 5 min
allow_postpone       = false
strict_break         = false  # if true, breaks can't be skipped or postponed
random_order         = true

[smart_pause]
enabled      = true
idle_seconds = 60           # pause after this many idle seconds

[audible_alert]
enabled         = true
pre_break       = true
post_break      = true
pre_break_sound = "Tink"    # names from /System/Library/Sounds
post_break_sound = "Glass"

[notification]
enabled = true             # show the pre-break Liquid Glass warning

[keybindings]
skip     = "escape"   # key that skips the warning/break while it's showing
postpone = "space"    # key that postpones it (needs allow_postpone)
```

### Keybindings

While the warning dialog or break screen is up, the Skip and Postpone buttons
each have a keyboard shortcut. A key spec is a single letter (`"s"`), a named key (`escape`/`esc`,
`space`, `enter`/`return`, `tab`, `delete`/`backspace`), or a combination with
modifiers (`cmd`, `shift`, `alt`/`option`, `ctrl`) joined by `+` — e.g.
`"cmd+s"`. The shortcut is shown as a dim hint on its button. An empty string
disables a keybind. A keybind only fires when its action is allowed: postpone
needs `allow_postpone`, and neither works under `strict_break`.

### Do not disturb

Suppress breaks while you're in the middle of something:

```toml
[do_not_disturb]
enabled = true

# No extra permission needed:
skip_when_app_running = ["zoom"]   # matched against app name & bundle id
skip_when_app_active  = []         # only when the app is frontmost

# Need Accessibility permission (you'll be prompted once):
skip_when_fullscreen  = true       # frontmost window is fullscreen
window_title_matches  = ["Zoom Meeting", "Plain"]  # patterns vs. window titles

action = "skip"                    # "skip" (advance) or "postpone" (retry later)
```

- `skip_when_app_running` / `skip_when_app_active` use `NSWorkspace` and need no
  special permission. `["zoom"]` covers Zoom being open at all.
- `window_title_matches` is the precise one: it reads the title of **every
  on-screen window** via the Accessibility API and suppresses the break if any
  matches. Each entry is a **case-insensitive** regex (a plain word like
  `"Plain"` is just a substring match). For example, with Zen showing the tab
  *"Re: Your receipt for Typesense Cloud - Plain"*, the entry `"Plain"` keeps
  breaks away while that tab is up; `"Zoom Meeting"` catches an active Zoom call.
- `skip_when_fullscreen` and `window_title_matches` need Accessibility — grant it
  in **System Settings → Privacy & Security → Accessibility**, then restart omma.
  Only the **active** tab's title is visible per window; background tabs of
  Firefox-based browsers (e.g. Zen) aren't readable.

## License

GPL-3.0-or-later.
