# Desktops

On Linux earwig reads the keyboard itself, so its shortcut works on any
desktop. That takes keyboard access: the NixOS module grants it
(`services.earwig.keyboardAccess`, on by default); elsewhere press **Allow**
under Keyboard access in the settings, which installs
[70-earwig.rules](../assets/udev/70-earwig.rules). Any program you run can
then read what you type. Where keyd already holds the keyboards, the shortcut
still works but other apps see it too, so pick a key nothing else uses.

GNOME and KDE ask once for permission to type for you; earwig keeps the
answer until you revoke it.

## Without keyboard access

In a sandbox like Flatpak, if you'd rather not allow it, or with
`[hotkey] key = ""`, the desktop delivers the shortcut.

GNOME and KDE ask on first start to confirm earwig's shortcuts (Ctrl+Space to
dictate, and a cancel one you pick); change them in the desktop's keyboard
settings.

Elsewhere, bind `earwig press` to a key going down and `earwig release` to it
coming up, or `earwig toggle` where binds fire on press only; `earwig cancel`
throws a recording away.

```kdl
// niri (26.04 and older have no release binds: spawn "earwig" "toggle")
Ctrl+Space {
    press { spawn "earwig" "press"; }
    release { spawn "earwig" "release"; }
}
```

```
# sway
bindsym --no-repeat Ctrl+space exec earwig press
bindsym --release Ctrl+space exec earwig release

# Hyprland
bind = CTRL, space, exec, earwig press
bindr = CTRL, space, exec, earwig release
```

On COSMIC, add a custom shortcut in Settings → Keyboard that runs
`earwig toggle`.
