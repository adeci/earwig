# Configuration

The settings window (`earwig settings`) saves to `config.toml` in
`~/.config/earwig`, `~/Library/Application Support/earwig` or
`%APPDATA%\earwig`; [config.rs](../crates/earwig-core/src/config.rs) has every
key. A `config.toml` in `/etc/xdg/earwig`, `/Library/Application Support/earwig`
or `%ProgramData%\earwig` applies to everyone and locks the keys it sets; the
NixOS and nix-darwin modules write it from `services.earwig.settings`.

Models come from the settings window or `earwig models` (`list`, `search`,
`download`, `use`, `remove`). The accuracy and speed dots are
[Handy](https://github.com/cjpais/Handy)'s measurements.

## Hooks

`filter` gets each transcript on stdin, and what it prints is inserted
instead; if it fails or takes longer than `filter_timeout_ms` (2000), the
transcript goes in as it was. `on_insert` gets the inserted text afterwards.
Both are commands as lists:

```toml
[hooks]
filter = ["sh", "-c", "sed 's/gonna/going to/g'"]
```

## Events

`earwig events` streams what the daemon does as JSON lines: the state, the
live transcript, each insertion, an update. `earwig status` prints one
snapshot. [events.rs](../crates/earwig-core/src/events.rs) is the format.

## Updates

The Windows release packages (the installer and the portable zip) and the
Mac app from the DMG look for a newer earwig a minute after they start, then
once a day. That is one request to GitHub's API,
`https://api.github.com/repos/adeci/earwig/releases/latest` (a pre-release
such as `0.1.0-rc.1` asks for `…/releases` and takes the newest), with
earwig's version in its User-Agent; nothing else is sent. **Check for
updates** in Settings → General turns it off, as does:

```toml
[updates]
check = false
```

A newer release puts **Update to …** in the tray and at the bottom of the
settings window; `earwig update` installs it from a terminal, `earwig update
--check` only asks. earwig downloads this system's file of the release over
HTTPS from where GitHub lists it, no more bytes than GitHub lists and within
30 minutes, and keeps it only if its SHA-256 matches the digest GitHub took
when it was uploaded. An older release is never installed. Then:

- **Windows**: the installer runs without a window. It stops earwig,
  replaces it and starts it again; **Start at sign-in** stays as it was. If
  the installer fails it starts earwig again, but files it had already
  replaced stay replaced. The installer isn't code-signed yet, so the update
  is as trustworthy as the GitHub release itself.
- **macOS**: the earwig.app in the disk image must be validly signed, all of
  it and strictly, so that it meets the running app's own designated
  requirement (the same app and Developer ID team), and be the newer version.
  It takes the old app's place in one step and starts, and keeps its
  Microphone and Accessibility permissions. If any of that fails, the
  installed app stays as it was.

The portable zip and a Mac app in a folder you can't write to only say a
newer release is out and link its page. Builds from Nix, cargo or a package
manager, and every Linux build, never check: what installed them updates
them.
