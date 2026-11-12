# Desktops

On Linux earwig reads the keyboard itself, so its hotkey (Super+Space unless
you record another in the settings) works the same on any desktop, Wayland or
X11. That needs keyboard access: the NixOS module allows it
(`services.earwig.keyboardAccess`, on by default); elsewhere press **Allow**
under Keyboard access in the settings, which installs
[70-earwig.rules](../assets/udev/70-earwig.rules) as
`/etc/udev/rules.d/70-earwig.rules` after asking for your password. The rule
lets the user at the seat read keyboards, which means any program you run
could read what you type.

earwig also keeps a hotkey with a key in it from other apps and the compositor
by holding the keyboards and passing every other key on through a virtual one.
Where it cannot, `earwig status` says why:

- keyd (or another remapper) already holds the keyboards: earwig watches keyd's
  virtual keyboard instead, so the hotkey works, but other apps see it too.
  Pick a key nothing else uses, or modifiers alone such as Right Ctrl.
- `[hotkey] block = false` turns holding off.

Without keyboard access (Flatpak, or you'd rather not), or with
`[hotkey] key = ""`, the hotkey comes from the desktop instead. The commands
below are all a binding needs: `earwig press` on key down and `earwig release`
on key up, `earwig toggle` where a binding can't see key releases, and
`earwig cancel` to throw a recording away.

## GNOME and KDE

On first start the desktop asks you to confirm earwig's shortcuts: Ctrl+Space
to dictate, and a cancel shortcut with no key until you pick one. Change either
in the desktop's keyboard shortcut settings.

The desktop also asks once for permission to type for you, and earwig keeps
the answer until you revoke it.

## niri

```kdl
Ctrl+Space {
    press { spawn "earwig" "press"; }
    release { spawn "earwig" "release"; }
}
```

niri 26.04 and older have no release binds, so the key can only toggle:
`Ctrl+Space { spawn "earwig" "toggle"; }`.

## sway

```
bindsym --no-repeat Ctrl+space exec earwig press
bindsym --release Ctrl+space exec earwig release
```

## Hyprland

```
bind = CTRL, space, exec, earwig press
bindr = CTRL, space, exec, earwig release
```

## COSMIC

Add a custom shortcut in Settings → Keyboard that runs `earwig toggle`.
COSMIC runs shortcuts on key down only, so tap to start and tap again to
finish.
