# Desktops

On Linux earwig reads the keyboard itself, so its shortcut works on any
desktop. That takes keyboard access: the NixOS module grants it
(`services.earwig.keyboardAccess`, on by default); elsewhere press **Allow**
under Keyboard access in the settings, which installs
[70-earwig.rules](../assets/udev/70-earwig.rules). Any program you run can
then read what you type. Where keyd already holds the keyboards, the shortcut
still works but other apps see it too, so pick a key nothing else uses.

earwig types the text into apps that take an input method on niri, sway,
Hyprland and COSMIC. Elsewhere it pastes: it puts the text on the clipboard
and in the primary selection, the one a middle click pastes, then presses
Shift+Insert, which editors, browsers and terminals all paste with, whatever
the keyboard layout. On GNOME and KDE it presses the keys on a keyboard of
its own, so nothing asks for permission; GNOME needs Xwayland for this, which
it runs by default. Both selections keep the text: a middle click pastes it
again, and what you had selected before no longer does.

## GNOME's top bar

GNOME has no tray of its own: earwig's icon needs the AppIndicator and
KStatusNotifierItem Support extension. The NixOS module installs it and
switches it on for everyone from their next login
(`services.earwig.gnomeTray`, on with GNOME). If you keep your own list of
enabled extensions, in home-manager for instance, add it there:
`dconf.settings."org/gnome/shell".enabled-extensions = [ pkgs.gnomeExtensions.appindicator.extensionUuid ];`.
Otherwise switch it on in the Extensions app, or run
`gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com`. Without the
icon, open the settings from the app grid or with `earwig settings`;
`earwig quit` stops earwig.

## Without keyboard access

In a sandbox like Flatpak, if you'd rather not allow it, or with
`[hotkey] key = ""`, the desktop delivers the shortcut.

GNOME and KDE ask when earwig starts: to confirm its shortcuts (Ctrl+Space to
dictate, and a cancel one you pick), press **Add** in GNOME's "Add Keyboard
Shortcuts" or **OK** in KDE's "Global Shortcuts Requested"; to let it type,
switch on **Allow Remote Interaction** and press **Share** in GNOME's "Remote
Desktop", or press **OK** in KDE's "Remote Control Requested". It then pastes
as it does with keyboard access, pressing Shift+Insert through the desktop.
earwig keeps the answers until you revoke them.

If you said no to typing, the next dictation says so, leaves the text on the
clipboard and has the desktop ask again. If you said no to the shortcut, give
Dictate one in Settings → Apps → earwig → Global Shortcuts on GNOME, System
Settings → Keyboard → Shortcuts on KDE.

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
