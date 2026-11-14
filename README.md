# <img src="assets/io.github.adeci.earwig.svg" width="48" alt="" align="absmiddle"> earwig

Local dictation for Linux, macOS and Windows, made with Rust and
[GPUI](https://www.gpui.rs). Hold a key and talk, or tap it to keep listening.
You see the words as you speak, and when you stop they land wherever you're
typing, all at once.

![The overlay while dictating, in the dark theme and in the light one](docs/overlay.avif)

## Setup

**Windows** 10 or 11: run the setup from
[Releases](https://github.com/adeci/earwig/releases), or unzip the portable
zip.

**NixOS**, and **macOS** through nix-darwin for now:

```nix
# inputs.earwig.url = "github:adeci/earwig";
imports = [ inputs.earwig.nixosModules.default ]; # darwinModules on a Mac
services.earwig.enable = true;
```

On a Mac, earwig asks for Microphone and Accessibility on first start and
after each update; allow both.

**Other Linux**: [build it from source](docs/building.md) (Ubuntu, Debian,
Fedora, Arch).

## Use

|         | Hold or tap |
| ------- | ----------- |
| Linux   | Super+Space |
| macOS   | ⌥ Space     |
| Windows | Ctrl+Space  |

`earwig settings` changes it ([configuration](docs/configuration.md)).
