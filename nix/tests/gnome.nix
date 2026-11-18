# Dictation end to end on GNOME (Wayland) without keyboard access: the GlobalShortcuts portal for
# the hotkey; for insertion earwig's own X11 selections and Shift+Insert through the RemoteDesktop
# portal (libei); the overlay as an override-redirect X11 popup (Mutter has no layer-shell). GNOME
# asks at start while earwig keeps answering; a dictation while typing is refused says what to
# allow and has GNOME ask again, so the next one lands, without a restart, once each in Text
# Editor, Console and foot. Keyboard access given while earwig runs takes insertion off the portal.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-gnome";
  enableOCR = true;

  nodes.machine = {
    imports = [
      common.machine
      common.gnome
    ];
    services.earwig.keyboardAccess = false;
    systemd.user.services.earwig.environment.RUST_LOG = "info,earwig_core=debug";
  };

  testScript = common.prelude + builtins.readFile ./gnome-shell.py + builtins.readFile ./gnome.py;
}
