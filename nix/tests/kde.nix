# Dictation end to end on KDE Plasma 6 (Wayland), chosen by capability only:
# KWin offers layer-shell and data-control but no input-method-v2 or virtual
# keyboard, so earwig inserts through the RemoteDesktop + Clipboard portals
# over libei and takes its hotkey from the GlobalShortcuts portal. The test
# accepts both consent dialogs once, taps a real key, and checks the text in
# KWrite and Konsole, the overlay as a layer-shell surface, and that the
# restore token skips the dialog after a daemon restart.
#
# Session setup follows nixpkgs nixos/tests/plasma6.nix.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-kde";
  enableOCR = true;

  nodes.machine =
    { pkgs, lib, ... }:
    {
      imports = [ common.machine ];

      # The fallback without keyboard access: the GlobalShortcuts portal.
      services.earwig.keyboardAccess = false;

      # The display manager logs alice in; a second autologin on tty1 would
      # only add a console session.
      services.getty.autologinUser = lib.mkForce null;

      services.xserver.enable = true;
      services.displayManager.plasma-login-manager.enable = true;
      services.desktopManager.plasma6.enable = true;
      services.displayManager.autoLogin = {
        enable = true;
        user = "alice";
      };
      environment.plasma6.excludePackages = with pkgs.kdePackages; [
        elisa
        # The welcome wizard would take focus on first login.
        plasma-welcome
      ];

      environment.systemPackages = [ pkgs.kdePackages.kate ];

      # No screen locking during the test.
      environment.etc."xdg/kscreenlockerrc".text = lib.generators.toINI { } {
        Daemon = {
          Autolock = false;
          LockOnResume = false;
        };
      };

      systemd.user.services.earwig.environment.RUST_LOG =
        "info,earwig=debug,earwig_core=debug,zbus=warn,tracing=warn";
    };

  testScript = common.prelude + builtins.readFile ./kde.py;
}
