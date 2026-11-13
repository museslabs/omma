# ὄμμα · omma

An eye-strain break reminder for macOS. It sits in the menu bar, schedules short
and long breaks, covers the screen with a countdown for each one, and can hold
breaks back while you're in a meeting or a fullscreen app.

omma is written in Rust with [`objc2`](https://github.com/madsmtm/objc2). Its
`unsafe` code is the interop with macOS frameworks, and each block says why it's
sound.

## Requirements

- macOS 26 or later. The warning and the break screen use Liquid Glass, and on
  an older system omma exits with `omma: requires macOS 26 or later`.
- Rust 1.89 or later, to build it.

## Install and run

Install it with Cargo, from crates.io or from a clone of this repo:

```sh
cargo install omma
cargo install --path .
```

Or download the Apple silicon build from the
[releases page](https://github.com/museslabs/omma/releases) and unpack it. It
isn't notarized, so if macOS won't run it, clear its quarantine flag:

```sh
tar -xzf omma-v0.1.0-aarch64-apple-darwin.tar.gz
cd omma-v0.1.0-aarch64-apple-darwin
xattr -d com.apple.quarantine omma   # only if macOS blocks it
./omma
```

Put the binary on your `PATH` to run it as `omma`. It runs as a menu-bar binary
with no Dock icon, and isn't packaged as an app bundle.

## What it does

- **Short and long breaks** on their own intervals: by default a short break
  every 15 minutes and a long one every 75.
- **A warning before each break**, a small panel by the menu-bar icon with a
  countdown and Skip and Postpone buttons. In strict mode Skip becomes Dismiss,
  which only closes the panel. The warning never takes keyboard focus from the
  app you're typing in.
- **A break screen** on every display, with the break's name, a countdown in a
  bead, optional background image, and Skip and Postpone buttons. The countdown
  starts once the screen has faded in, so a break runs about 0.6 s longer than
  its length, or 0.25 s with Reduce Motion on. omma never becomes the active
  app, and your app gets the keyboard back when the break ends.
- **A menu** with the time to the next break, items to take a break now (the
  next one, a short one or a long one), Enabled, Open config… and Quit. It also
  lists anything that needs fixing; see
  [When something's wrong](#when-somethings-wrong).
- **Sounds** before a break and after one that ran to its end.
- **Smart pause**: the schedule pauses while you're away from the keyboard.
- **Do not disturb**: rules that hold breaks back while you're busy.
- **A CLI** that controls the running instance.

## Configuration

The config file is `$XDG_CONFIG_HOME/omma/config.toml`, or
`~/.config/omma/config.toml` when `XDG_CONFIG_HOME` isn't set. omma creates it
with the defaults on first run, and "Open config…" creates it again if it's
gone. `omma --print-default-config` prints the default file, which documents
every key with its range.

omma rereads the file a second or two after every save, during a break too. A
warning or break already on screen keeps the buttons and shortcuts it has until
it closes, and one the new config forbids does nothing. Parsing is strict:

- A key omma doesn't know is an error. So is a top-level key placed under a
  `[table]` header, and the error says where it belongs.
- Times are whole seconds within a range: intervals and `postpone_duration`
  1 s to 24 h, break durations 1 s to 1 h, `pre_break_warning` 0 to 10 min, and
  `idle_seconds` 10 s to 24 h. With both kinds of break,
  `short_break_interval` must be less than `long_break_interval`.
- A break `image` must be an absolute path or start with `~/`, and the file must
  exist when the config loads.

On an error the menu shows it, with its line when it has one, and omma keeps the
last config that loaded, or the defaults at startup. Fix the file and save, and
the error goes.

Each break in the `[[short_breaks]]` and `[[long_breaks]]` lists has a `name`,
and can set its own `duration` and `image`:

```toml
[[long_breaks]]
name     = "Take a walk"
duration = 300
image    = "~/Pictures/trail.jpg"
```

### Keybindings

```toml
[keybindings]
skip     = "escape"   # on the break screen
postpone = "space"
warning_skip     = ""  # on the warning, from any app
warning_postpone = ""
```

`skip` and `postpone` work on the break screen once its buttons have appeared,
a little over a second in, so a key you were typing as the break began doesn't
skip it.

`warning_skip` and `warning_postpone` are off by default. They work from any app
while the warning shows, as system-wide shortcuts, so the warning never needs
focus. Each needs `cmd` or `ctrl`, for example `"cmd+shift+option+space"`.

A shortcut is one key after any of the modifiers `cmd`, `shift`, `alt` (or
`option`) and `ctrl`, joined by `+`: a single character, or `escape`, `space`,
`enter`, `tab` or `delete` (`esc`, `return` and `backspace` work too). Shift
goes only with a letter, `escape`, `space`, `enter` or `delete`. `""` turns a
shortcut off. With `allow_postpone` on, skip and postpone must differ. A
shortcut that breaks these rules is a config error. Postpone shortcuts need
`allow_postpone`, and with `strict_break` on no shortcut works.

### Smart pause

```toml
[smart_pause]
enabled      = true
idle_seconds = 60
```

After `idle_seconds` without keyboard or mouse input the schedule pauses. The
idle time counts from your last input, so the threshold counts toward your rest.
Once you're back the next break is a full interval away, and time away at least
as long as a long break counts as one. Sleep counts as time away, so waking the
Mac never brings on a break at once.

### Do not disturb

```toml
[do_not_disturb]
enabled = true
action  = "skip"     # or "postpone"

# need no permission
skip_when_app_running = ["zoom"]
skip_when_app_active  = []          # only while the app is frontmost

# need Accessibility permission
skip_when_fullscreen = false        # while the frontmost window is fullscreen
window_title_matches = ["Zoom Meeting", '\bPlain\b']
```

omma checks the rules when a break comes due. When one holds, `action = "skip"`
drops the break and `"postpone"` tries again after `postpone_duration`, even with
`allow_postpone` off or `strict_break` on. The menu then says why, for example
"Skipped the last break: Zoom is running", while omma waits for the next break.

- `skip_when_app_running` and `skip_when_app_active` match part of an app's name
  or bundle id, ignoring case. `"zoom"` covers Zoom being open at all.
- `window_title_matches` holds a break while a window on screen has a matching
  title. Minimized windows and hidden apps don't count. Each entry is a regex
  that ignores case, so a plain word matches anywhere in a title: `"Plain"`
  also matches "explain". A TOML literal string with word boundaries,
  `'\bPlain\b'`, matches the word alone. A browser window shows only its
  active tab's title.
- Each request omma sends an app gives up after 0.25 s. An app that's hung, or
  slower than that, counts as having no matching window.

The fullscreen and window-title rules need Accessibility permission. omma asks
for it when it starts with one of them on, or when a save turns one on. Grant it
in System Settings → Privacy & Security → Accessibility (Device Control and Data
Access on macOS 27), and the rules work from
the next break, with no restart. Started from a terminal, omma uses the
terminal's permission instead of its own.

## When something's wrong

The menu lists problems under the status line, each marked ⚠. A long one is cut
short, and hovering it shows all of it:

- an error in the config file; clicking it opens the file;
- a break image that couldn't be shown; clicking it opens the config file;
- a warning shortcut the system wouldn't register;
- a fullscreen or window-title rule without Accessibility permission; clicking
  it opens System Settings at the Accessibility list.

`omma --status` lists them too, and omma writes each one to stderr.

## CLI

```text
omma                          start the menu-bar app
omma --take-break  (-t)       take the next break now
omma --short-break (-s)       take a short break now
omma --long-break  (-l)       take a long break now
omma --enable      (-e)       enable the running instance
omma --disable     (-d)       disable the running instance
omma --quit        (-q)       quit the running instance
omma --status                 print the running instance's status
omma --reload                 reread the config now
omma --print-default-config   print the default config
```

Flags can't be combined. Each command waits for the running instance to act on
it, then prints what it did or why it refused. The exit code is:

- 0 when the command worked;
- 1 when omma isn't running, or, when starting it, is already running;
- 2 when the running instance refused the command (a break while omma is
  disabled, say, or `--reload` with a broken config), or the flags are invalid;
- 3 when anything else went wrong.

The running instance holds a lock file and listens on a socket, both in
`~/Library/Application Support/omma/`.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
