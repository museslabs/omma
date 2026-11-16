# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- omma runs on Linux, on Wayland compositors with the layer-shell protocol, such
  as Hyprland, Sway, niri, river and KDE Plasma.
- On Linux, the tray icon and its menu use StatusNotifierItem.
- On Linux, the warning and the break screen look and move as they do on macOS.
  OpenGL ES draws them. If the compositor blurs what is behind a surface, they
  are glass. If not, they are darker.
- On Linux, smart pause uses the idle notifications of the compositor, and the
  window rules use its list of windows.
- On Linux, omma follows the dark, high-contrast and reduced-motion settings of
  the desktop portal.
- `Tink` and `Glass` are built-in sounds on Linux, so one config works on both
  systems.
- On Linux, the global shortcuts of the warning use the desktop portal. The
  portal needs the desktop file in `linux/`.
- `linux/omma.service` is a systemd user service that starts omma with the
  graphical session.
- `omma --skip` and `omma --postpone` skip or postpone the break that the warning
  counts down to, or the break on screen. A key binding of the compositor can
  run them in place of a global shortcut.

## [0.1.0] - 2026-11-13

The first release.

### Added

- Short and long breaks come on their own intervals. A warning comes before each
  break, and a break screen with a countdown covers every display.
- Skip and Postpone have keyboard shortcuts on the break screen. Optional
  system-wide shortcuts (`warning_skip`, `warning_postpone`) work on the warning.
- Strict mode, smart pause, sounds, durations for each break, and background
  images.
- Do-not-disturb rules hold breaks back for running apps, frontmost apps,
  fullscreen windows and window titles. The menu and `omma --status` say which
  rule held a break back.
- The menu pauses omma for 30 minutes, one hour, two hours or the rest of the
  day. `omma --pause 45m` pauses it for up to 24 hours. omma comes back on its
  own.
- A CLI controls the running instance, with an exit code for each result.
- The menu and `omma --status` show problems. These are config errors and break
  images that omma cannot show. They also include warning shortcuts that the
  system does not register, and window rules without Accessibility permission.

### Changed since the pre-release builds

- omma requires macOS 26 or later. On an older system, it says so at startup.
- omma reads the config strictly. Unknown keys, keys under the wrong table, times
  out of range and shortcuts that break the rules are errors.
- A break `image` that is not an existing file at an absolute path or a `~/` path
  is an error.
- On a config error, omma shows the error in the menu. It keeps the last good
  config, or the defaults at startup.
- The `interval` key of a break is gone. Each break keeps its own `duration` and
  `image`.
- `[audible_alert]` is now `[sounds]`, with two keys, `pre_break` and
  `post_break`. Each key names a sound, or is `""` for none. A name with no sound
  is an error.
- `[notification]` is gone. `pre_break_warning = 0` turns the warning off.
- `[do_not_disturb]` has no `enabled` key, because empty rules hold nothing back.
  Its rules are now `apps_running`, `apps_in_front`, `fullscreen` and
  `window_titles`.
- `shift` in a shortcut goes only with a letter, `escape`, `space`, `enter` or
  `delete`. The `delete` key works.
- The CLI rejects combined flags. Each command prints what it did or why it
  refused, in place of "ok". The CLI exits with 0, 1, 2 or 3.
- The CLI socket moved from `$TMPDIR/omma.sock` to
  `~/Library/Application Support/omma/omma.sock`. A lock file next to it stops a
  second instance from starting.
- omma never becomes the active app. The warning takes no keyboard focus. The
  break screen gives the keyboard back to your app when it ends.
- The shortcuts of the break screen work after its buttons appear.
- The countdown starts after the break screen fades in. With animations on, a
  break runs about 0.6 s longer.
- A config change no longer restarts the schedule. It applies during a warning
  or a break too.
- A warning or break that is already on screen keeps its buttons and shortcuts
  after a config change. If the new config forbids one of them, it does nothing.
- omma asks for Accessibility permission at startup, and when a save turns a
  fullscreen or window-title rule on. It no longer asks on every save. A grant
  takes effect without a restart.
- The window-title rule ignores minimized windows and hidden apps.
- "Open config…" creates the config file again if it is gone. If no app opens
  `.toml` files, it opens the file in the default text editor.

### Fixed since the pre-release builds

- A skip, postpone or take-break that you clicked during a fade sometimes applied
  to the next break.
- After sleep or a clock change, a break sometimes came at once on wake.
- Idle time did not include the threshold, so a rest counted for less than its
  real length.
- Long breaks drifted later than `long_break_interval`.
- Very large config values sometimes crashed omma.
- Two instances sometimes started at the same time.
- A slow CLI client held up the other clients.
- An app that did not answer froze omma for seconds when a break came due.
- An app that answered a do-not-disturb rule with unexpected data sometimes
  crashed omma.
- After you closed the terminal that started omma, omma sometimes crashed on its
  next message.
- With Reduce Motion on, the warning still moved.

### Known issues

- During the swap animation of the countdown, the new digit scales off-center.
- If you turn omma off while a break screen fades in, the screen jumps to full
  opacity before it fades out.
- An app that takes longer than 0.25 s to answer counts as having no matching
  window. A busy app can therefore miss a window-title rule.

[Unreleased]: https://github.com/museslabs/omma/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/museslabs/omma/releases/tag/v0.1.0
