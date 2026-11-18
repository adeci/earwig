# <img src="assets/io.github.adeci.earwig.svg" width="48" alt="" align="absmiddle"> earwig

Local dictation for Linux, macOS and Windows, made with Rust and
[GPUI](https://www.gpui.rs).

![The overlay while dictating, in the dark theme and in the light one](docs/overlay.avif)

## Install

### NixOS and nix-darwin

```nix
# flake input: earwig.url = "github:adeci/earwig";
imports = [ inputs.earwig.nixosModules.default ]; # darwinModules.default on macOS
services.earwig.enable = true;
```

The module installs earwig with its default speech model and starts it with
your session. On nix-darwin, macOS asks for Microphone and Accessibility on the
first start.

### Other Linux distributions

[Build from source](docs/building.md). The steps are tested on Ubuntu, Debian,
Fedora and Arch.

### macOS

Requires an Apple silicon Mac with macOS 14 or later.

1. Download `earwig-<version>-macos-arm64.dmg` from the
   [latest release](https://github.com/adeci/earwig/releases/latest).
2. Open it and drag earwig to Applications.
3. Start earwig and allow Microphone and Accessibility when macOS asks.
4. Open **Settings** from the menu bar icon and download a model under
   **Models**.

### Windows

Requires Windows 10 (version 1809 or later) or Windows 11 on a 64-bit Intel or
AMD processor with AVX2 (Intel Core 4th generation, AMD Ryzen, or newer).

1. Download `earwig-<version>-windows-x64-setup.exe` from the
   [latest release](https://github.com/adeci/earwig/releases/latest).
2. Run it. It installs for the current user and needs no administrator rights.
3. In the settings window that opens, download a model under **Models**.

The installer and the programs it installs are signed. Windows may still warn
about a new release until enough people have downloaded it: choose
**More info**, then **Run anyway**. A portable `.zip` is on the same page.

## Usage

earwig runs in the background, with an icon in the system tray or the menu
bar. Hold the shortcut while you speak, or tap it to start and tap it again to
finish. The text is inserted into the application you are typing in. Speech is
transcribed on your machine: on the GPU through Vulkan or Metal when it has
one, otherwise on the CPU.

| Platform | Default shortcut |
| -------- | ---------------- |
| Linux    | Super+Space      |
| macOS    | ⌥ Space          |
| Windows  | Ctrl+Space       |

The settings window, opened from the icon or with `earwig settings`, changes
the shortcut, the microphone and the speech model.
[Configuration](docs/configuration.md) covers the config file, hooks and the
event stream.

## Updates

On Windows and macOS, earwig checks GitHub for a new release once a day and
offers **Update to …** in the tray menu and the settings window. Installations
from Nix or from source don't check so just update them with those tools.

## Acknowledgements

Speech recognition is
[transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) from the
[Handy](https://github.com/cjpais/Handy) project. The speech models are
NVIDIA's and others', each under its own licence; earwig downloads Handy's
conversions of them and shows Handy's measurements in its model list.

earwig was built with the assistance of large language models.
