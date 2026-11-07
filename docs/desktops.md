# Desktops

On Linux, how earwig gets its hotkey depends on the desktop. The commands
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
