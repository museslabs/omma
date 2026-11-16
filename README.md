# omma

> **omma** (/'o.ma/): from Greek *ὄμμα*, eye.

An eye-strain break reminder for the macOS menu bar and Wayland desktops, written
in Rust.

<img width="1200" height="675" alt="omma-example" src="https://github.com/user-attachments/assets/3f0ea20c-6a7c-4870-8aea-bc578ba1f2f3" />

omma counts down to your next break in the menu bar or the tray. Just before a
break, a bead drops on a thread with a warning. Then the break covers every
display with a countdown. omma can hold breaks back while you are in a meeting or
in a fullscreen app. The schedule pauses while you are away from the keyboard.

On macOS, omma needs macOS 26 or later, because the warning and the break screen
use Liquid Glass. On an older system, omma stops with
`omma: requires macOS 26 or later`.

On Linux, omma needs a Wayland compositor that has the layer-shell protocol.
Hyprland, Sway, niri, river and KDE Plasma have it. GNOME does not, and there
omma stops with an error that says so. The tray icon needs a StatusNotifierItem
tray, such as the `tray` module of Waybar or the KDE panel.

The warning and the break screen look as they do on macOS. Some compositors blur
what is behind a surface (the `ext-background-effect` protocol, which Hyprland
has). There, the warning and the break screen are glass. On other compositors,
they are darker, so that the text stays easy to read over a sharp desktop.

## Install

Install with Homebrew:

```sh
brew tap museslabs/omma
brew install omma
```

Install with cargo (Rust 1.89 or later):

```sh
cargo install omma
```

On Linux, the build links against the Wayland, xkbcommon and EGL libraries. On
Debian or Ubuntu, install their headers before you build:

```sh
sudo apt install pkg-config libwayland-dev libxkbcommon-dev libegl-dev
```

On Arch Linux, the `wayland`, `libxkbcommon` and `mesa` packages supply them.

Build from source:

```sh
git clone https://github.com/museslabs/omma
cd omma
cargo build --release
```

You can also download the build for Apple silicon or for x86_64 Linux from the
[releases page](https://github.com/museslabs/omma/releases), and unpack it. The
macOS build is not notarized. If macOS does not run it, remove its quarantine
flag:

```sh
tar -xzf omma-v0.1.0-aarch64-apple-darwin.tar.gz
cd omma-v0.1.0-aarch64-apple-darwin
xattr -d com.apple.quarantine omma   # only if macOS blocks it
```

omma is a single binary with no Dock icon, not an app bundle. Put it on your
`PATH` to run it as `omma`.

On Linux, omma plays its sounds with `pw-play`, `paplay` or `aplay`, whichever is
installed. It opens files and links with `xdg-open`.

## Setup

### Start at login on macOS

If you installed omma with Homebrew, run:

```sh
brew services start omma
```

If not, save this file as `~/Library/LaunchAgents/com.museslabs.omma.plist`. Use
the full path to your `omma` binary and to a log file, because launchd does not
expand `~`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.museslabs.omma</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/.cargo/bin/omma</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>Crashed</key>
    <true/>
  </dict>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardErrorPath</key>
  <string>/Users/you/Library/Logs/omma.log</string>
</dict>
</plist>
```

Load the file. This also starts omma now:

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.museslabs.omma.plist
```

To stop omma from starting at login, run:

```sh
launchctl bootout gui/$(id -u)/com.museslabs.omma
rm ~/Library/LaunchAgents/com.museslabs.omma.plist
```

launchd restarts omma only after a crash. If a second copy finds omma already
running, the second copy stops and stays stopped.

When launchd starts omma, omma does not see the environment of your shell. If you
set `XDG_CONFIG_HOME`, add it to the plist. If you do not, omma reads
`~/.config/omma/config.toml`.

```xml
<key>EnvironmentVariables</key>
<dict>
  <key>XDG_CONFIG_HOME</key>
  <string>/Users/you/.config</string>
</dict>
```

When launchd starts omma, omma also stops using the Accessibility permission of
your terminal. Allow omma itself for the fullscreen and window-title rules. See
[Accessibility](#accessibility).

macOS shows a "Background Items Added" notice the first time.

### Start at login on Linux

Start omma from the config of your compositor. For Hyprland, add this line:

```text
exec-once = omma
```

For Sway, add `exec omma`. For river, add `riverctl spawn omma`.

You can also start omma with the graphical session, as a systemd user service.
[`linux/omma.service`](linux/omma.service) is a service file for this:

1. Copy the file to `~/.config/systemd/user/omma.service`.
2. In the copy, set `ExecStart` to the full path of your `omma` binary.
3. Enable and start the service:

   ```sh
   systemctl --user enable --now omma
   ```

### The desktop file

The desktop portal knows apps by their desktop file. It gives global shortcuts
only to an app that it knows. To use [`warning_skip` and
`warning_postpone`](#keybindings) on Linux, install
[`linux/com.museslabs.omma.desktop`](linux/com.museslabs.omma.desktop):

```sh
cp linux/com.museslabs.omma.desktop ~/.local/share/applications/
```

The portal accepts the desktop file only if it finds the program that the file
runs. If `omma` is not in `/usr/bin` or `/usr/local/bin`, set the `Exec` line to
the full path, for example `Exec=/home/you/.cargo/bin/omma`.

### Accessibility

On macOS, the fullscreen and window-title rules of
[do not disturb](#do-not-disturb) need Accessibility permission. omma asks for it
when it starts with one of these rules on, and when a save turns one on. Grant it
in System Settings → Privacy & Security → Accessibility (Device Control and Data
Access on macOS 27). The rules then work from the next break, with no restart. If
you start omma from a terminal, omma uses the permission of the terminal.

Released binaries are not signed with a Developer ID, so macOS treats each
upgrade as a different program. After an upgrade, allow omma again in the same
list. Until you do, the menu shows a ⚠ item for it.

On Linux, there is no permission to grant. The compositor tells apps about its
windows, or it does not. The fullscreen, window-title and `apps_in_front` rules
need the `wlr-foreign-toplevel-management` protocol. Hyprland, Sway, niri, river,
labwc and Wayfire have it. With only `ext-foreign-toplevel-list`, the window-title
rule works on its own. With neither protocol, the menu shows a ⚠ item for the
rules that cannot work.

## Usage

Run `omma`. Its icon shows in the menu bar or the tray. By default, a 20-second
break comes every 15 minutes, and a one-minute break comes every 75 minutes. The
icon blinks in the minute before each break.

### Breaks

Before each break, a bead drops on a thread from the icon in the menu bar. A
small panel next to the bead counts down and has Skip and Postpone buttons. In
strict mode, Skip becomes Dismiss, which only closes the warning. The warning
never takes keyboard focus from the app that you type in. `pre_break_warning = 0`
turns the warning off. You can grab the bead and swing it.

A Wayland app cannot find where its tray icon is. On Linux, the bead hangs from
the bottom of the bar at the top right of the display in use, where a tray
usually is.

The break screen covers every display. It shows the name of the break and a
countdown in a bead. The color of the bead drains as the break runs. The screen
also shows an optional background image, and Skip and Postpone buttons. The
countdown starts after the screen fades in. This adds about 0.6 s to a break, or 0.25 s with Reduce Motion on. omma
never becomes the active app, and your app gets the keyboard back when the break
ends.

On Linux, the settings of the desktop portal take the place of the macOS
settings. omma follows the dark or light preference of the desktop. It reduces
motion when the desktop asks for reduced motion or turns animations off. It draws
solid surfaces for high contrast.

A sound plays when the warning shows and when a break runs to its end. The
`[sounds]` table of the config picks the sounds. On macOS, a sound is a system
sound by name, such as `Tink`. On Linux, `Tink` and `Glass` are built in. Any
other name is a sound from the freedesktop theme, such as `bell` or `complete`,
or a sound that you add to `~/.local/share/sounds`.

### The menu

The menu shows the time to the next break. Its items take a break now: the next
one, a short one or a long one. Pause stops omma for 30 minutes, one hour, two
hours or the rest of the day. Enabled turns omma on and off. Other items open
the config file and the docs, show the version, and quit omma. The menu also
lists problems. See [When something is wrong](#when-something-is-wrong).

### CLI

```text
omma                          start omma in the menu bar or the tray
omma --take-break  (-t)       take the next break now
omma --short-break (-s)       take a short break now
omma --long-break  (-l)       take a long break now
omma --skip                   skip the break the warning counts down to, or the one on screen
omma --postpone               postpone it
omma --enable      (-e)       enable the running instance
omma --disable     (-d)       disable the running instance
omma --pause <DURATION>       pause for 90s, 30m, 2h30m and so on, up to 24h
omma --quit        (-q)       quit the running instance
omma --status                 print the running instance's status
omma --reload                 reread the config now
omma --print-default-config   print the default config
```

You cannot combine flags. Each command waits for the running instance to act on
it. Then the command prints what it did, or why the instance refused. The exit
code is one of these:

- 0: the command worked.
- 1: omma is not running. When you start omma, 1 means that omma is already
  running.
- 2: the running instance refused the command, or the flags are not valid. For
  example, the instance refuses a break while omma is disabled, and `--reload`
  with a broken config.
- 3: something else went wrong.

`--skip` and `--postpone` do what the buttons do. Bind them to keys in your
compositor to use them in place of global shortcuts.

The running instance holds a lock file and listens on a socket. On macOS, both
are in `~/Library/Application Support/omma/`. On Linux, both are in
`$XDG_RUNTIME_DIR/omma/`.

### When something is wrong

The menu lists problems under the status line, each marked ⚠. If a problem is
long, the menu cuts it short, and its tooltip shows all of it. Linux tray menus
have no tooltips. The menu lists these problems:

- An error in the config file. Click it to open the file.
- A break image that omma failed to show. Click it to open the config file.
- A warning shortcut that the system did not register.
- On macOS, a fullscreen or window-title rule without Accessibility permission.
  Click it to open the Accessibility list in System Settings.
- On Linux, a window rule that the compositor cannot support. Click it to open
  the docs.

`omma --status` lists the problems too, and omma writes each one to stderr.

## Configuration

The config file is `$XDG_CONFIG_HOME/omma/config.toml`. If `XDG_CONFIG_HOME` is
not set, the file is `~/.config/omma/config.toml`. omma creates the file with the
defaults when it first runs. "Open config…" creates the file again if it is gone.
`omma --print-default-config` prints the default file, which documents each key
and its range.

omma reads the file again a second or two after each save, also during a break.
A warning or break that is already on screen keeps its buttons and shortcuts
until it closes. If the new config forbids one of them, it does nothing. omma
reads the file strictly:

- A key that omma does not know is an error. A top-level key under a `[table]`
  header is also an error, and the error says where the key belongs.
- Times are whole seconds within a range. Intervals and `postpone_duration` are
  1 s to 24 h. Break durations are 1 s to 1 h. `pre_break_warning` is 0 to 10 min,
  and `idle_seconds` is 10 s to 24 h. With both kinds of break,
  `short_break_interval` must be less than `long_break_interval`.
- A break `image` must be an absolute path or start with `~/`. The file must
  exist when the config loads.

If the file has an error, the menu shows it, with its line number when it has
one. omma keeps the last config that loaded, or the defaults at startup. When you
correct the file and save it, the error goes away.

Each break in the `[[short_breaks]]` and `[[long_breaks]]` lists has a `name`. It
can also set its own `duration` and `image`:

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

`skip` and `postpone` work on the break screen after its buttons appear, a little
over one second into the break. A key that you were typing when the break began
does not skip it.

`warning_skip` and `warning_postpone` are off by default. They are system-wide
shortcuts that work from any app while the warning shows, because the warning
never takes focus. Each one needs `cmd` or `ctrl`, for example
`"cmd+shift+option+space"`.

On Linux, `super` is another name for `cmd`, and both work. omma gets the
shortcuts from the desktop portal, which needs [the desktop file](#the-desktop-file).
KDE asks you to confirm the shortcuts. Hyprland lists them as
`com.museslabs.omma:skip` and `com.museslabs.omma:postpone`, and you bind them
with its `global` dispatcher. If your compositor has no portal, bind
`omma --skip` and `omma --postpone` to keys instead. For Sway, add these lines to
its config:

```text
bindsym $mod+Shift+s exec omma --skip
bindsym $mod+Shift+p exec omma --postpone
```

A shortcut is one key after any of the modifiers `cmd` (or `super`), `shift`,
`alt` (or `option`) and `ctrl`, joined by `+`. The key is a single character, or
`escape`, `space`, `enter`, `tab` or `delete`. `esc`, `return` and `backspace`
also work. Shift goes only with a letter, `escape`, `space`, `enter` or `delete`.

`""` turns a shortcut off. With `allow_postpone` on, the skip and postpone
shortcuts must differ. A shortcut that breaks these rules is a config error.
Postpone shortcuts need `allow_postpone`. With `strict_break` on, no shortcut
works.

### Smart pause

```toml
[smart_pause]
enabled      = true
idle_seconds = 60
```

After `idle_seconds` without keyboard or mouse input, the schedule pauses. Your
rest counts from your last input, and includes the threshold. When you come
back, the next break is a full interval away. Time away that is at least as long
as a long break counts as a long break. Sleep counts as time away too, and the
computer never starts a break as soon as it wakes.

On Linux, smart pause needs a compositor with the `ext-idle-notify` protocol,
which most compositors have. As on macOS, smart pause looks only at your input.
A video that keeps the screen awake does not stop omma from seeing that you are
away.

### Do not disturb

```toml
[do_not_disturb]
action = "skip"   # or "postpone"

# need no permission
apps_running  = ["zoom"]
apps_in_front = []        # only while the app is frontmost

# need Accessibility permission
fullscreen    = false     # while the frontmost window is fullscreen
window_titles = ["Zoom Meeting", '\bPlain\b']
```

omma looks at the rules when a break comes due. If a rule holds,
`action = "skip"` drops the break, and `"postpone"` tries again after
`postpone_duration`. This happens even with `allow_postpone` off or
`strict_break` on. The menu then says why, for example "Skipped the last break:
Zoom is running", while omma waits for the next break.

- `apps_running` and `apps_in_front` match part of the name or the bundle ID of
  an app, and ignore case. `"zoom"` matches when Zoom is open at all. On Linux,
  they match part of the app ID of a window. `apps_running` also matches part of
  the name of a process.
- `window_titles` holds a break back while a window on screen has a matching
  title. Minimized windows and hidden apps do not count. Each entry is a regular
  expression that ignores case. A plain word matches anywhere in a title, so
  `"Plain"` also matches "explain". A TOML literal string with word boundaries,
  `'\bPlain\b'`, matches the word alone. A browser window shows only the title
  of its active tab.
- On macOS, each request that omma sends to an app stops after 0.25 s. An app
  that hangs, or that answers more slowly, counts as having no matching window.
  On Linux, the compositor answers for all apps at once.

On macOS, the fullscreen and window-title rules need Accessibility permission.
On Linux, these two rules and `apps_in_front` need a compositor that lists its
windows. See [Accessibility](#accessibility).

## Credits

omma takes its default schedule and most of its break prompts from
[Safe Eyes](https://github.com/slgobinath/SafeEyes). The eye prompts follow the
[20-20-20 rule](https://www.aao.org/eye-health/diseases/what-is-eye-strain)
instead of eye rotations. According to the American Academy of Ophthalmology, eye
rotations
[do not help](https://www.aao.org/eye-health/ask-ophthalmologist-q/why-do-patches-help-vision-not-eye-exercises).

omma talks to macOS through [`objc2`](https://github.com/madsmtm/objc2). It talks
to Wayland through [`wayland-rs`](https://github.com/Smithay/wayland-rs), with
[`glutin`](https://github.com/rust-windowing/glutin) for EGL,
[`cosmic-text`](https://github.com/pop-os/cosmic-text) for text,
[`ksni`](https://github.com/iovxw/ksni) for the tray and
[`zbus`](https://github.com/dbus2/zbus) for the desktop portal. Its `unsafe` code
is this interop and the GL calls. Each `unsafe` block says why it is sound.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
