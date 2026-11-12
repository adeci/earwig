# <img src="assets/io.github.adeci.earwig.svg" width="48" alt="" align="absmiddle"> earwig

Local dictation for Linux, macOS and Windows. Hold a key and talk, or tap it
to keep listening. You see the words as you speak, and when you stop they land
wherever you're typing, all at once.

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

On macOS, with nix-darwin:

```nix
imports = [ inputs.earwig.darwinModules.default ];

services.earwig.enable = true;
```

When it first starts, and again after an update (macOS ties the permissions to
the exact build), earwig asks for Microphone and Accessibility: allow both in
System Settings → Privacy & Security.

Elsewhere, `nix profile install github:adeci/earwig` and start `earwig daemon`
with your session.

On Windows 10 or 11, run `earwig-<version>-windows-x64-setup.exe` from the
[releases](https://github.com/adeci/earwig/releases) (or unzip the portable
zip) and start earwig from the Start menu.

## Hotkey

Hold or tap the hotkey: Super+Space on Linux, Option+Space on macOS,
Ctrl+Space on Windows; record another in the settings. On Linux earwig reads
the keyboard itself once allowed, as [docs/desktops.md](docs/desktops.md)
explains.

## Models

Pick a model in the settings window (`earwig settings`): click one to use it,
and it downloads first if it isn't on your computer yet. Dots rate each
curated model's accuracy and speed, as measured by
[Handy](https://github.com/cjpais/Handy). Typing in the search box filters
the list; the row at its end searches Hugging Face for untested models. A
GGUF file of your own goes in `config.toml` as `model = "/path/to/model.gguf"`
under `[model]`. From a terminal, `earwig models`.

## Customizing

Settings are in `~/.config/earwig/config.toml`; the keys are in
[config.rs](crates/earwig-core/src/config.rs). A `[hooks] filter` command can
rewrite each transcript before it's inserted, for word replacements or an LLM
cleanup pass. `earwig events` streams what the daemon is doing as JSON lines.
