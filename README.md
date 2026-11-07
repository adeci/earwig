# <img src="assets/io.github.adeci.earwig.svg" width="48" alt="" align="absmiddle"> earwig

Local dictation for Linux and Windows. Hold a key and talk, or tap it to keep
listening. You see the words as you speak, and when you stop they land wherever
you're typing, all at once.

![The overlay while dictating, in the dark theme and in the light one](docs/overlay.webp)

If the overlay is in the way, drag it aside; the next dictation brings it
back. The × on it throws the dictation away. It powers on and off like an
old TV: when you stop, it switches off at once while your words land; if
they can't be inserted, it comes back to say so.

## Setup

On NixOS:

```nix
imports = [ inputs.earwig.nixosModules.default ];

services.earwig.enable = true;
```

Elsewhere, `nix profile install github:adeci/earwig` and start `earwig daemon`
with your session.

On Windows 10 or 11, run `earwig-<version>-windows-x64-setup.exe` from the
[releases](https://github.com/adeci/earwig/releases) (or unzip the portable
zip) and start earwig from the Start menu.

## Hotkey

Hold or tap the hotkey: Right Ctrl on Windows (pick another in the settings),
Ctrl+Space on Linux. GNOME and KDE set it up when earwig first starts;
elsewhere, bind it as [docs/desktops.md](docs/desktops.md) shows.

## Models

Pick a model in the settings window (`earwig settings`). It lists a few
recommended ones and searches Hugging Face for anything else that works. From
a terminal, `earwig models`.

## Customizing

Settings are in `~/.config/earwig/config.toml`; the keys are in
[config.rs](crates/earwig-core/src/config.rs). A `[hooks] filter` command can
rewrite each transcript before it's inserted, for word replacements or an LLM
cleanup pass. `earwig events` streams what the daemon is doing as JSON lines.
