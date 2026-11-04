# AGENTS.md

earwig is a local dictation daemon for Linux desktops: hold or tap a hotkey,
speak, and the text lands in the focused app in one step. Rust, GPUI for the
overlay and settings window, transcribe.cpp for speech. NixOS first.

## Layout

- `crates/earwig-core`: everything but pixels. Config, tap-or-hold state
  machine, audio, model catalog/downloads, insertion backends, hotkey
  sources, control socket, controller. No GPUI dependency, so its tests are
  fast.
- `crates/earwig-transcribe`: the `EngineFactory` for transcribe.cpp (GGUF,
  Vulkan + CPU compiled in).
- `crates/earwig`: the binary. CLI, daemon wiring, GPUI overlay and settings
  window, tray.
- `crates/earwig-core/models.toml`: curated models shipped with a release.
- `nix/`: package, NixOS module, dev shell, treefmt, VM tests.

Cross-module contracts live in `earwig-core/src/{events,engine,config,
desktop}.rs`, `models/mod.rs`, `insert/mod.rs`, and `hotkey/mod.rs`. Change
them deliberately; every front end depends on them.

## Develop

```bash
nix develop
cargo test -p earwig-core
cargo clippy --workspace --all-targets
cargo run -p earwig -- --help
nix fmt
nix flake check -L        # package, clippy, tests, VM tests (needs KVM)
```

## Rules

- Audio handed to an engine is 16 kHz mono `f32`.
- Final text is inserted once, atomically. Never type it character by
  character.
- The daemon never logs transcript text above `debug`.
- Extension happens through the `EngineFactory`/`Inserter`/`HotkeySource`
  traits, `[hooks]` commands, and the event socket. Keep those small and
  documented instead of growing special cases.
- Clippy pedantic is on; fix warnings at the cause.
