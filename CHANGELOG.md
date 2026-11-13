# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - unreleased

The first release.

### Added

- Short and long breaks on their own intervals, with a warning before each one
  and a break screen with a countdown on every display.
- Skip and Postpone, with keyboard shortcuts on the break screen, and optional
  system-wide shortcuts (`warning_skip`, `warning_postpone`) for the warning.
- Strict mode, smart pause, sounds, per-break durations and background images.
- Do-not-disturb rules for running or frontmost apps, fullscreen windows and
  window titles. The menu and `omma --status` say which rule held a break back.
- A CLI to control the running instance, with an exit code for each outcome.
- Problems show in the menu and in `omma --status`: config errors, a break image
  that can't be shown, a warning shortcut the system won't register, and window
  rules without Accessibility permission.

### Changed since the pre-release builds

- omma requires macOS 26 or later, and says so at startup on an older system.
- The config is parsed strictly. Unknown keys, keys under the wrong table,
  times out of range, shortcut specs that break the rules, and a break `image`
  that isn't an existing file at an absolute or `~/` path are all errors. omma
  shows the error in the menu and keeps the last good config, or the defaults at
  startup.
- The per-break `interval` key is gone. Breaks keep their own `duration` and
  `image`.
- `[audible_alert]` is now `[sounds]`, with two keys, `pre_break` and
  `post_break`. Each names a sound, or is `""` for none, and a name with no
  sound behind it is an error.
- `shift` in a shortcut goes only with a letter, `escape`, `space`, `enter` or
  `delete`, and the `delete` key works.
- The CLI rejects combined flags. Each command prints what it did or why it
  refused instead of "ok", and exits with 0, 1, 2 or 3.
- The CLI socket moved from `$TMPDIR/omma.sock` to
  `~/Library/Application Support/omma/omma.sock`, next to a lock file that keeps
  a second instance from starting.
- omma never becomes the active app. The warning takes no keyboard focus, and
  the break screen gives the keyboard back to your app when it ends. The break
  screen's shortcuts work once its buttons appear.
- The countdown starts once the break screen has faded in, so with animations on
  a break runs about 0.6 s longer.
- A config change no longer restarts the schedule, and applies during a warning
  or break, though a warning or break already on screen keeps the buttons and
  shortcuts it has, and one the new config forbids does nothing.
- Accessibility is asked for at startup and when a save turns a fullscreen or
  window-title rule on, not on every save, and a grant takes effect without a
  restart.
- The window-title rule leaves out minimized windows and hidden apps.
- "Open config…" recreates a missing config file, and opens it in the default
  text editor when no app is set for `.toml` files.

### Fixed since the pre-release builds

- A skip, postpone or take-break clicked during a fade could apply to the next
  break.
- After sleep or a clock change, a break could come at once on wake.
- Idle time left out the threshold, so a rest counted for less than it was.
- Long breaks drifted later than `long_break_interval`.
- Very large config values could crash omma.
- Two instances could start at once, and a slow CLI client held up the others.
- An unresponsive app could freeze omma for seconds when a break came due.
- An app that answered a do-not-disturb rule with unexpected data could crash
  omma.
- Closing the terminal that started omma could make it crash on its next
  message.
- With Reduce Motion on, the warning still animated.

### Known issues

- During the countdown's swap animation the new digit scales off-centre.
- Turning omma off while a break screen fades in makes the screen jump to full
  opacity before it fades out.
- An app that takes longer than 0.25 s to answer counts as having no matching
  window, so a busy app can miss a window-title rule.

[0.1.0]: https://github.com/museslabs/omma/releases/tag/v0.1.0
