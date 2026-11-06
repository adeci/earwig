# AGENTS.md

earwig is a local dictation daemon for Linux desktops and macOS: hold or tap a hotkey,
speak, and the text lands in the focused app. Rust, GPUI for the overlay and
settings window, transcribe.cpp for speech. NixOS first.

## Layout

- `crates/earwig-core`: everything but pixels (config, controller, audio,
  models, insertion, hotkeys, control socket). No GPUI, so tests are fast.
- `crates/earwig-transcribe`: the transcribe.cpp `EngineFactory` (GGUF,
  CPU plus Vulkan, or Metal on macOS).
- `crates/earwig`: the binary: CLI, daemon wiring, GPUI overlay, settings
  window, tray.
- `crates/earwig-core/models.toml`: the curated model catalog.
- `nix/`: package, NixOS module, model fetchers, dev shell, treefmt, VM tests;
  `nix/darwin/`: earwig.app and the nix-darwin module.
- `tests/fixtures/`: speech clips for engine and VM tests.
- `.github/workflows/ci.yml`: fmt, clippy, tests and a release build on macOS
  and Windows; Linux is the flake's checks. `macos-app.yml` builds earwig.app
  from the flake and screenshots the overlay over a dictation.

Per-OS code lives in `linux`, `macos` and `windows` modules behind one
signature each: `desktop/`, `hotkey/`, `insert/` and `paths/` in
earwig-core, `ui/platform/` in earwig; `ipc/` and `hooks/` split `unix`
from `windows`.

Contracts every front end depends on: `earwig-core/src/{events,engine,
config}.rs`, `desktop/mod.rs`, `models/mod.rs`, `insert/mod.rs`,
`hotkey/mod.rs`. Change them deliberately. `events.rs` is also the
control-socket wire format that `earwig status`/`earwig events` print.

## Commands

```bash
nix develop
cargo test --workspace
cargo clippy --workspace --all-targets
nix fmt
nix flake check -L                                # package, clippy, tests, VM tests (KVM)
nix build .#checks.x86_64-linux.vm-<name> -L      # one VM test: niri sway gnome kde cosmic models
```

## Rules

- Audio handed to an engine is 16 kHz mono `f32`.
- Final text is inserted once, atomically, never typed character by character.
- The daemon never logs transcript text above `debug`.
- Extend through the `EngineFactory`/`Inserter`/`HotkeySource` traits,
  `[hooks]`, and the event socket, not special cases.
- VM tests assert observable behavior (`earwig status`/`events`, files,
  windows, OCR), not log wording.
- Clippy pedantic is on; fix warnings at the cause.
