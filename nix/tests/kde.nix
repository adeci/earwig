# Dictation end to end on Plasma 6 without keyboard access: KWin has no input-method-v2, so
# insertion goes through the RemoteDesktop portal and the hotkey through GlobalShortcuts; the
# restore token must skip the consent dialog after a restart. Setup as in nixpkgs' plasma6 test.
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
