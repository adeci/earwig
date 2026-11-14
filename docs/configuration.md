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
live transcript, each insertion. `earwig status` prints one snapshot.
[events.rs](../crates/earwig-core/src/events.rs) is the format.
