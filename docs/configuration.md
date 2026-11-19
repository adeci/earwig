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

While the model in use is not on disk, earwig opens the settings window on
**Models** each time it starts, and its shortcut shows the overlay saying so
instead of listening. Nothing is downloaded until you click **Download** (or
run `earwig models download`); dictation works once it is in. `earwig status`
says the model is `missing`, and `earwig daemon --headless` opens nothing. A
`[model] model` path to a file that isn't there is said to be missing too, but
the settings window doesn't open for it and nothing is downloaded in its place.

## Discard

The shortcut with Shift added throws the dictation in progress away: add
Shift while you hold the shortcut, or press both after a tap. While nothing is
being dictated, other apps get those keys as usual. Set another shortcut, or
none, under **Discard** in the settings window or as `[hotkey] discard` (`""`
for none); a shortcut with Shift in it has none until you set one.
`earwig cancel` does the same, as does the cancel shortcut GNOME and KDE ask
for without keyboard access.

## Replacements

A table of what earwig heard and what it writes instead, for the names it
keeps getting wrong:

```toml
[replacements]
"get hub" = "GitHub"
"nix os" = "NixOS"
```

Add to it in the settings window, under **Replacements** or from a transcript
in **History**, or with `earwig replacements` (`list`,
`add <heard> <written>`, `remove <heard>`). A running earwig uses the change
from its next dictation.

The heard phrase matches whole words only, in any capitals and with any
spaces between them, and the punctuation around it stays: `Get hub,` becomes
`GitHub,` and `Get hub's` becomes `GitHub's`, but a contraction is one word,
so `it` is not heard in `it's` nor `can` in `can't`. Chinese, Thai, kanji and
hiragana, written without spaces, match anywhere; a run of katakana is one
word. What is written goes in exactly as entered. Where two phrases overlap
the longer one wins, and what a replacement wrote is not replaced again. The
live text shows what was heard; the final text is replaced before the
`filter` hook gets it.

Entries of the system file are locked one by one, and yours apply beside
them:

```nix
services.earwig.settings.replacements."nix os" = "NixOS";
```

## Overlay position

The overlay opens at the screen edge `[overlay] position` names; drag it away
and the next dictation still starts at that edge. Turn on **Remember
position** (`remember_position = true`) and it opens where you last left it,
after a restart too, moved just enough to stay on the screen if that one is
smaller. The spot is kept in `overlay.json` in earwig's state directory, not
in your settings; turning the setting off forgets it.

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
such as `x.y.z-rc.1` asks for `…/releases` and takes the newest), with
earwig's version in its User-Agent; nothing else is sent. **Check for
updates** in Settings → General turns it off, as does:

```toml
[updates]
check = false
```

A newer release puts **Update to …** in the tray and at the bottom of the
settings window; clicking it installs the release. From a terminal,
`earwig update` installs it and `earwig update --check` only asks. earwig
downloads this system's file of the release over HTTPS from where GitHub
lists it, no more bytes than GitHub lists and within 30 minutes, and keeps it
only if its SHA-256 matches the digest GitHub took when it was uploaded. An
older release is never installed. Then:

- **Windows**: a signed earwig runs the installer only if Windows finds its
  Authenticode signature valid and it comes from earwig's own publisher: a
  certificate of the same Artifact Signing certificate profile (the EKU
  under 1.3.6.1.4.1.311.97 that every day's new certificate carries),
  chaining to the same root certificate as earwig's own signature. If the
  installer is not signed by earwig's own publisher (as after the publisher's
  certificate identity changes), or Windows cannot reach the certificate
  authority to check that a certificate was not revoked, earwig refuses the
  update and says why; download the installer from the release page and run
  it yourself. An unsigned earwig, one packaged from source outside CI, takes
  the installer on its SHA-256 alone. The installer runs without a window. It
  stops earwig, replaces it and starts it again; **Start at sign-in** stays
  as it was. If the installer fails it starts earwig again, but files it had
  already replaced stay replaced.
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
